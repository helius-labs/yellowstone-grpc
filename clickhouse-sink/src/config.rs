use serde::Deserialize;

#[derive(Debug, Clone, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ClickhouseConfig {
    pub url: String,
    #[serde(default = "default_user")]
    pub user: String,
    #[serde(default)]
    pub password: String,
    #[serde(default = "default_database")]
    pub database: String,
    #[serde(default)]
    pub env: Option<String>,
    #[serde(default)]
    pub host: Option<String>,
}

fn default_user() -> String {
    "default".to_string()
}

fn default_database() -> String {
    "default".to_string()
}