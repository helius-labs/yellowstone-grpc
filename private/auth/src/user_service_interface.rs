use async_trait::async_trait;
use serde::Deserialize;

#[async_trait]
pub trait UserServiceInterface {
    async fn get_api_key(&self, api_key: String) -> Result<String, String>;
}

#[derive(Debug, Deserialize)]
#[serde(rename_all(serialize = "camelCase", deserialize = "camelCase"))]
pub struct ApiKeyDetails {
    pub project_id: String,
    pub project_name: Option<String>,
    pub api_key: ApiKey,
    pub subscription: Subscription,
    pub users: Vec<String>,
}

#[derive(Debug, Deserialize)]
#[serde(rename_all(serialize = "camelCase", deserialize = "camelCase"))]
pub struct ApiKey {
    pub key_id: String,
    pub project_id: Option<String>,
    pub name: Option<String>,
    pub allow_cidr: Option<Vec<String>>,
    pub allow_domain: Option<Vec<String>>,
    pub allow_ip: Option<Vec<String>>,
    pub created_at: String,
    pub last_updated_at: String,
}

#[derive(Debug, Deserialize)]
#[serde(rename_all(serialize = "camelCase", deserialize = "camelCase"))]
pub struct Subscription {
    pub id: String,
    pub billing_anchor: String,
    pub project_id: Option<String>,
    pub stripe_customer_id: Option<String>,
    pub auto_scaling_credit_limit: f64,
    pub crypto_sub: Option<bool>,
    pub plan_expires_at: Option<String>,
    pub plan: String,
}
