use {
    crate::{config::ConfigGrpc, version::GrpcVersionInfo},
    anyhow::Context,
    futures::task::Context as TaskContext,
    futures::Stream,
    log::info,
    std::{
        pin::Pin,
        sync::{
            atomic::{AtomicUsize, Ordering},
            Arc, RwLock as RwLockStd,
        },
        task::Poll,
    },
    tokio::sync::Notify,
    tonic::{
        service::interceptor::interceptor,
        transport::{
            server::{Server, TcpIncoming},
            Identity, ServerTlsConfig,
        },
        Request, Response, Result as TonicResult, Status, Streaming,
    },
    tonic_health::server::health_reporter,
    yellowstone_grpc_proto::{
        plugin::{
            filter::message::{FilteredUpdate, FilteredUpdateOneof},
            message::Message,
            proto::geyser_server::{Geyser, GeyserServer},
        },
        prelude::{
            GetBlockHeightRequest, GetBlockHeightResponse, GetLatestBlockhashRequest,
            GetLatestBlockhashResponse, GetSlotRequest, GetSlotResponse, GetVersionRequest,
            GetVersionResponse, IsBlockhashValidRequest, IsBlockhashValidResponse, PingRequest,
            PongResponse, SubscribeRequest,
        },
    },
};

#[derive(Debug)]
pub struct GrpcService {
    subscribe_id: AtomicUsize,
    raw_client_channels: Arc<RwLockStd<Vec<(u64, crossbeam_channel::Sender<Message>)>>>,
}

impl GrpcService {
    pub async fn create(
        config: ConfigGrpc,
        raw_client_channels: Arc<RwLockStd<Vec<(u64, crossbeam_channel::Sender<Message>)>>>,
        _is_reload: bool,
    ) -> anyhow::Result<Arc<Notify>> {
        // Bind service address
        let incoming = TcpIncoming::new(
            config.address,
            true,                                     // tcp_nodelay
            Some(std::time::Duration::from_secs(20)), // tcp_keepalive
        )
        .map_err(|error| anyhow::anyhow!(error))?;

        // gRPC server builder with optional TLS
        let mut server_builder = Server::builder();
        if let Some(tls_config) = &config.tls_config {
            let (cert, key) = tokio::try_join!(
                tokio::fs::read(&tls_config.cert_path),
                tokio::fs::read(&tls_config.key_path)
            )
            .context("failed to load tls_config files")?;
            server_builder = server_builder
                .tls_config(ServerTlsConfig::new().identity(Identity::from_pem(cert, key)))
                .context("failed to apply tls_config")?;
        }
        if let Some(enabled) = config.server_http2_adaptive_window {
            server_builder = server_builder.http2_adaptive_window(Some(enabled));
        }
        if let Some(http2_keepalive_interval) = config.server_http2_keepalive_interval {
            server_builder =
                server_builder.http2_keepalive_interval(Some(http2_keepalive_interval));
        }
        if let Some(http2_keepalive_timeout) = config.server_http2_keepalive_timeout {
            server_builder = server_builder.http2_keepalive_timeout(Some(http2_keepalive_timeout));
        }
        if let Some(sz) = config.server_initial_connection_window_size {
            server_builder = server_builder.initial_connection_window_size(sz);
        }
        if let Some(sz) = config.server_initial_stream_window_size {
            server_builder = server_builder.initial_stream_window_size(sz);
        }

        // Create Server
        let max_decoding_message_size = config.max_decoding_message_size;
        let mut service = GeyserServer::new(Self {
            subscribe_id: AtomicUsize::new(0),
            raw_client_channels,
        })
        .max_decoding_message_size(max_decoding_message_size);
        for encoding in config.compression.accept {
            service = service.accept_compressed(encoding);
        }
        for encoding in config.compression.send {
            service = service.send_compressed(encoding);
        }

        // Run Server
        let shutdown = Arc::new(Notify::new());
        let shutdown_grpc = Arc::clone(&shutdown);
        tokio::spawn(async move {
            // gRPC Health check service
            let (mut health_reporter, health_service) = health_reporter();
            health_reporter.set_serving::<GeyserServer<Self>>().await;

            server_builder
                .layer(interceptor(move |request: Request<()>| {
                    if let Some(x_token) = &config.x_token {
                        match request.metadata().get("x-token") {
                            Some(token) if x_token == token => Ok(request),
                            _ => Err(Status::unauthenticated("No valid auth token")),
                        }
                    } else {
                        Ok(request)
                    }
                }))
                .add_service(health_service)
                .add_service(service)
                .serve_with_incoming_shutdown(incoming, shutdown_grpc.notified())
                .await
        });

        Ok(shutdown)
    }
}

pub struct CrossbeamReceiverStream {
    inner: crossbeam_channel::Receiver<Message>,
    raw_client_channels: Arc<RwLockStd<Vec<(u64, crossbeam_channel::Sender<Message>)>>>,
    client_id: u64,
}

impl CrossbeamReceiverStream {
    fn new(
        inner: crossbeam_channel::Receiver<Message>,
        raw_client_channels: Arc<RwLockStd<Vec<(u64, crossbeam_channel::Sender<Message>)>>>,
        client_id: u64,
    ) -> Self {
        Self {
            inner,
            raw_client_channels,
            client_id,
        }
    }
}

impl Drop for CrossbeamReceiverStream {
    fn drop(&mut self) {
        // Remove client from the vec by ID
        if let Ok(mut raw_clients) = self.raw_client_channels.write() {
            if let Some(pos) = raw_clients.iter().position(|(id, _)| *id == self.client_id) {
                raw_clients.swap_remove(pos);
                info!(
                    "removed raw client {}, remaining: {}",
                    self.client_id,
                    raw_clients.len()
                );
            }
        }
    }
}

impl Stream for CrossbeamReceiverStream {
    type Item = TonicResult<FilteredUpdate>;

    fn poll_next(self: Pin<&mut Self>, cx: &mut TaskContext<'_>) -> Poll<Option<Self::Item>> {
        const MAX_QUEUE_SIZE: usize = 1_000_000;

        if self.inner.len() > MAX_QUEUE_SIZE {
            info!(
                "Raw client {} queue too large ({}), disconnecting",
                self.client_id,
                self.inner.len()
            );
            return Poll::Ready(None);
        }

        match self.inner.try_recv() {
            Ok(message) => {
                // Convert raw Message to FilteredUpdate
                let filtered_update = match &message {
                    Message::Account(msg) => FilteredUpdate::new_empty(
                        FilteredUpdateOneof::account(msg, Default::default()),
                    ),
                    Message::Slot(msg) => {
                        FilteredUpdate::new_empty(FilteredUpdateOneof::slot(msg.clone()))
                    }
                    Message::Transaction(msg) => {
                        FilteredUpdate::new_empty(FilteredUpdateOneof::transaction(msg))
                    }
                    Message::Entry(msg) => {
                        FilteredUpdate::new_empty(FilteredUpdateOneof::entry(Arc::clone(msg)))
                    }
                    Message::BlockMeta(msg) => {
                        FilteredUpdate::new_empty(FilteredUpdateOneof::block_meta(Arc::clone(msg)))
                    }
                    _ => {
                        unreachable!("will never receive block messages in raw mode")
                    }
                };
                Poll::Ready(Some(Ok(filtered_update)))
            }
            Err(crossbeam_channel::TryRecvError::Empty) => {
                // No data available, wake up when more data might be available
                cx.waker().wake_by_ref();
                Poll::Pending
            }
            Err(crossbeam_channel::TryRecvError::Disconnected) => Poll::Ready(None),
        }
    }
}

#[tonic::async_trait]
impl Geyser for GrpcService {
    type SubscribeRawStream = CrossbeamReceiverStream;

    async fn subscribe_raw(
        &self,
        _request: Request<Streaming<SubscribeRequest>>,
    ) -> TonicResult<Response<Self::SubscribeRawStream>> {
        let (raw_message_tx, raw_message_rx) = crossbeam_channel::unbounded();

        // Register raw client channel with unique ID
        let client_id = self.subscribe_id.fetch_add(1, Ordering::Relaxed) as u64;
        if let Ok(mut raw_clients) = self.raw_client_channels.write() {
            raw_clients.push((client_id, raw_message_tx));
            info!(
                "registered raw client {}, total: {}",
                client_id,
                raw_clients.len()
            );
        } else {
            return Err(Status::internal("failed to register raw client"));
        }

        Ok(Response::new(CrossbeamReceiverStream::new(
            raw_message_rx,
            self.raw_client_channels.clone(),
            client_id,
        )))
    }

    async fn ping(&self, _request: Request<PingRequest>) -> Result<Response<PongResponse>, Status> {
        Err(Status::unimplemented("method disabled"))
    }

    async fn get_latest_blockhash(
        &self,
        _request: Request<GetLatestBlockhashRequest>,
    ) -> Result<Response<GetLatestBlockhashResponse>, Status> {
        Err(Status::unimplemented("method disabled"))
    }

    async fn get_block_height(
        &self,
        _request: Request<GetBlockHeightRequest>,
    ) -> Result<Response<GetBlockHeightResponse>, Status> {
        Err(Status::unimplemented("method disabled"))
    }

    async fn get_slot(
        &self,
        _request: Request<GetSlotRequest>,
    ) -> Result<Response<GetSlotResponse>, Status> {
        Err(Status::unimplemented("method disabled"))
    }

    async fn is_blockhash_valid(
        &self,
        _request: Request<IsBlockhashValidRequest>,
    ) -> Result<Response<IsBlockhashValidResponse>, Status> {
        Err(Status::unimplemented("method disabled"))
    }

    async fn get_version(
        &self,
        _request: Request<GetVersionRequest>,
    ) -> Result<Response<GetVersionResponse>, Status> {
        Ok(Response::new(GetVersionResponse {
            version: serde_json::to_string(&GrpcVersionInfo::default()).unwrap(),
        }))
    }
}
