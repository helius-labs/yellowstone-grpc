use {
    crate::{config::ConfigGrpc, version::GrpcVersionInfo},
    anyhow::Context,
    log::info,
    std::sync::{
        atomic::{AtomicUsize, Ordering},
        Arc,
    },
    tokio::sync::{mpsc, Notify},
    tokio_stream::wrappers::UnboundedReceiverStream,
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
pub enum ClientCommand {
    Subscribe {
        client_id: u64,
        sender: mpsc::UnboundedSender<Message>,
    },
    Unsubscribe {
        client_id: u64,
    },
    Broadcast {
        message: Message,
    },
}

#[derive(Debug)]
pub struct GrpcService {
    subscribe_id: AtomicUsize,
    client_command_tx: crossbeam_channel::Sender<ClientCommand>,
}

impl GrpcService {
    pub async fn create(
        config: ConfigGrpc,
        client_command_tx: crossbeam_channel::Sender<ClientCommand>,
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
            client_command_tx,
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

pub struct MessageReceiverStream {
    inner: UnboundedReceiverStream<Message>,
    client_command_tx: crossbeam_channel::Sender<ClientCommand>,
    client_id: u64,
}

impl MessageReceiverStream {
    fn new(
        inner: mpsc::UnboundedReceiver<Message>,
        client_command_tx: crossbeam_channel::Sender<ClientCommand>,
        client_id: u64,
    ) -> Self {
        Self {
            inner: UnboundedReceiverStream::new(inner),
            client_command_tx,
            client_id,
        }
    }
}

impl Drop for MessageReceiverStream {
    fn drop(&mut self) {
        // Send unsubscribe command
        let _ = self.client_command_tx.send(ClientCommand::Unsubscribe {
            client_id: self.client_id,
        });
        info!("sent unsubscribe command for client {}", self.client_id);
    }
}

impl futures::Stream for MessageReceiverStream {
    type Item = TonicResult<FilteredUpdate>;

    fn poll_next(
        mut self: std::pin::Pin<&mut Self>,
        cx: &mut std::task::Context<'_>,
    ) -> std::task::Poll<Option<Self::Item>> {
        use futures::StreamExt;

        self.inner.poll_next_unpin(cx).map(|message| {
            let Some(message) = message else {
                return None;
            };

            let filtered_update = match &message {
                Message::Account(msg) => {
                    FilteredUpdate::new_empty(FilteredUpdateOneof::account(msg, Default::default()))
                }
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

            Some(Ok(filtered_update))
        })
    }
}

#[tonic::async_trait]
impl Geyser for GrpcService {
    type SubscribeRawStream = MessageReceiverStream;

    async fn subscribe_raw(
        &self,
        _request: Request<Streaming<SubscribeRequest>>,
    ) -> TonicResult<Response<Self::SubscribeRawStream>> {
        let (raw_message_tx, raw_message_rx) = mpsc::unbounded_channel();

        // Register raw client channel with unique ID
        let client_id = self.subscribe_id.fetch_add(1, Ordering::Relaxed) as u64;

        if self
            .client_command_tx
            .send(ClientCommand::Subscribe {
                client_id,
                sender: raw_message_tx,
            })
            .is_err()
        {
            return Err(Status::internal("failed to register raw client"));
        }

        info!("registered raw client {}", client_id);

        Ok(Response::new(MessageReceiverStream::new(
            raw_message_rx,
            self.client_command_tx.clone(),
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
