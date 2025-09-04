use clickhouse::Client;

pub mod config;
pub mod event;
pub mod sink;

pub use config::ClickhouseConfig;

pub async fn init(config: ClickhouseConfig) -> anyhow::Result<()> {
    let client = Client::default()
        .with_url(&config.url)
        .with_user(&config.user)
        .with_password(&config.password)
        .with_database(&config.database)
        .with_option("allow_experimental_json_type", "1")
        .with_option("input_format_binary_read_json_as_string", "1");

    event::init(client, config.env, config.host).await?;

    log::info!("ClickHouse sink initialized");
    Ok(())
}
