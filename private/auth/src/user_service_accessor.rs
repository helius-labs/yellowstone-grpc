use async_trait::async_trait;

use crate::user_service_interface::{ApiKeyDetails, UserServiceInterface};

pub struct UserServiceAccessor {
    url: String,
    auth_token: String,
    client: reqwest::Client,
}

impl UserServiceAccessor {
    pub fn new(url: String, auth_token: String) -> Self {
        UserServiceAccessor {
            url: url,
            auth_token: auth_token,
            client: reqwest::Client::new(),
        }
    }
}

static AUTH_HEADER: &str = "x-api-key";

#[async_trait]
impl UserServiceInterface for UserServiceAccessor {
    async fn get_api_key(&self, api_key: String) -> Result<String, String> {
        // let resp = self
        //     .client
        //     .get(format!("{}/api-keys/{}?detail=true", self.url, api_key))
        //     .header(AUTH_HEADER, self.auth_token.clone())
        //     .send();
        // match resp {
        //     Ok(resp) => match resp.status() {
        //         reqwest_wasm::StatusCode::OK => return Ok(api_key),
        //         _ => return Err("f".to_string()),
        //     },
        //     Err(e) => {
        //         return Err(e.to_string());
        //     }
        // }
        Ok("".to_string())
    }
}

// return fetch(`${userServiceUrl}/api-keys/${apiKey}?detail=true`, {
//     method: 'GET',
//     headers: { 'Content-Type': 'application/json', 'x-api-key': userServiceAuthHeader },
// });
