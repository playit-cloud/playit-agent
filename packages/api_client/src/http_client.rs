use std::panic::Location;
use std::sync::Arc;

use reqwest::StatusCode;
use serde::Serialize;
use serde::de::DeserializeOwned;
use tokio::sync::RwLock;

use crate::api::{ApiResult, PlayitHttpClient};

#[derive(Clone)]
pub struct HttpClient {
    api_base: String,
    auth_header: Arc<RwLock<Option<String>>>,
    client: reqwest::Client,
}

impl HttpClient {
    pub fn new(api_base: String, auth_header: Option<String>) -> Self {
        HttpClient {
            api_base,
            auth_header: Arc::new(RwLock::new(auth_header)),
            client: reqwest::Client::new(),
        }
    }

    pub fn api_base(&self) -> &str {
        &self.api_base
    }

    pub async fn remove_auth(&self) {
        let mut lock = self.auth_header.write().await;
        let _ = lock.take();
    }
}

#[cfg(test)]
mod tests {
    use super::HttpClient;

    #[tokio::test]
    async fn clone_while_auth_is_write_locked_shares_auth_state() {
        let client = HttpClient::new(
            "https://example.invalid".to_string(),
            Some("initial".to_string()),
        );

        let mut auth = client.auth_header.write().await;
        let cloned = client.clone();
        *auth = Some("updated".to_string());
        drop(auth);

        assert_eq!(
            cloned.auth_header.read().await.as_deref(),
            Some("updated"),
            "cloning must never silently drop authentication"
        );

        cloned.remove_auth().await;
        assert_eq!(*client.auth_header.read().await, None);
    }
}

impl PlayitHttpClient for HttpClient {
    type Error = HttpClientError;

    async fn call<Req: Serialize + Send, Res: DeserializeOwned, Err: DeserializeOwned>(
        &self,
        _caller: &'static Location<'static>,
        path: &str,
        req: Req,
    ) -> Result<ApiResult<Res, Err>, Self::Error> {
        let mut builder = self.client.post(format!("{}{}", self.api_base, path));

        {
            let lock = self.auth_header.read().await;

            if let Some(auth_header) = &*lock {
                builder = builder.header(reqwest::header::AUTHORIZATION, auth_header);
            }
        }

        let res = async move {
            builder = builder.json(&req);
            let request = builder.build()?;

            let response = self.client.execute(request).await?;

            let response_status = response.status();
            if response_status == StatusCode::TOO_MANY_REQUESTS {
                return Err(HttpClientError::TooManyRequests);
            }

            let response_txt = response.text().await?;
            let result: ApiResult<Res, Err> = serde_json::from_str(&response_txt).map_err(|e| {
                tracing::error!("failed to parse json:\n{}", response_txt);
                HttpClientError::ParseError(e, response_status, response_txt.to_string())
            })?;

            Ok::<_, Self::Error>(result)
        }
        .await;

        if let Err(error) = &res {
            tracing::error!(?error, request = %std::any::type_name::<Req>(), "API call failed");
        }

        res
    }
}

#[derive(Debug, thiserror::Error)]
pub enum HttpClientError {
    #[error("failed to serialize API request: {0}")]
    SerializeError(serde_json::Error),
    #[error("failed to parse API response with status {1}: {0}")]
    ParseError(serde_json::Error, StatusCode, String),
    #[error("HTTP request failed: {0}")]
    RequestError(reqwest::Error),
    #[error("the API rate limit was exceeded")]
    TooManyRequests,
}

impl From<reqwest::Error> for HttpClientError {
    fn from(value: reqwest::Error) -> Self {
        HttpClientError::RequestError(value)
    }
}
