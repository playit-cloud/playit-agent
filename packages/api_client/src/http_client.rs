use std::panic::Location;
use std::time::Duration;

use reqwest::StatusCode;
use serde::Serialize;
use serde::de::DeserializeOwned;
use tokio::sync::RwLock;

use crate::api::{ApiResult, PlayitHttpClient};

pub struct HttpClient {
    api_base: String,
    auth_header: RwLock<Option<String>>,
    client: reqwest::Client,
}

const MAX_REQUEST_ATTEMPTS: usize = 3;
const RETRY_DELAY: Duration = Duration::from_millis(250);

impl Clone for HttpClient {
    fn clone(&self) -> Self {
        Self {
            api_base: self.api_base.clone(),
            auth_header: match self.auth_header.try_read() {
                Ok(v) => RwLock::new(v.clone()),
                _ => RwLock::new(None),
            },
            client: self.client.clone(),
        }
    }
}

impl HttpClient {
    pub fn new(api_base: String, auth_header: Option<String>) -> Self {
        HttpClient {
            api_base,
            auth_header: RwLock::new(auth_header),
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

impl PlayitHttpClient for HttpClient {
    type Error = HttpClientError;

    async fn call<Req: Serialize + Send, Res: DeserializeOwned, Err: DeserializeOwned>(
        &self,
        _caller: &'static Location<'static>,
        path: &str,
        req: Req,
    ) -> Result<ApiResult<Res, Err>, Self::Error> {
        let body = serde_json::to_value(req).map_err(HttpClientError::SerializeError)?;
        let res = async move {
            for attempt in 0..MAX_REQUEST_ATTEMPTS {
                let mut builder = self.client.post(format!("{}{}", self.api_base, path));

                {
                    let lock = self.auth_header.read().await;

                    if let Some(auth_header) = &*lock {
                        builder = builder.header(reqwest::header::AUTHORIZATION, auth_header);
                    }
                }

                let request = builder.json(&body).build()?;
                let response = match self.client.execute(request).await {
                    Ok(response) => response,
                    Err(error)
                        if attempt + 1 < MAX_REQUEST_ATTEMPTS
                            && (error.is_connect() || error.is_timeout()) =>
                    {
                        tracing::debug!(
                            attempt = attempt + 1,
                            max_attempts = MAX_REQUEST_ATTEMPTS,
                            ?error,
                            "retrying transient API request failure"
                        );
                        tokio::time::sleep(RETRY_DELAY * (attempt as u32 + 1)).await;
                        continue;
                    }
                    Err(error) => return Err(HttpClientError::RequestError(error)),
                };

                let response_status = response.status();
                let response_txt = response.text().await?;

                if (response_status == StatusCode::TOO_MANY_REQUESTS
                    || response_status.is_server_error())
                    && attempt + 1 < MAX_REQUEST_ATTEMPTS
                {
                    tracing::debug!(
                        attempt = attempt + 1,
                        max_attempts = MAX_REQUEST_ATTEMPTS,
                        status = %response_status,
                        "retrying transient API response"
                    );
                    tokio::time::sleep(RETRY_DELAY * (attempt as u32 + 1)).await;
                    continue;
                }

                if response_status == StatusCode::TOO_MANY_REQUESTS {
                    return Err(HttpClientError::TooManyRequests);
                }

                let result: ApiResult<Res, Err> =
                    serde_json::from_str(&response_txt).map_err(|e| {
                        tracing::error!("failed to parse json:\n{}", response_txt);
                        HttpClientError::ParseError(e, response_status, response_txt)
                    })?;

                return Ok(result);
            }

            unreachable!("request loop always returns after the final attempt")
        }
        .await;

        if let Err(error) = &res {
            tracing::error!(?error, request = %std::any::type_name::<Req>(), "API call failed");
        }

        res
    }
}

#[derive(Debug)]
pub enum HttpClientError {
    SerializeError(serde_json::Error),
    ParseError(serde_json::Error, StatusCode, String),
    RequestError(reqwest::Error),
    TooManyRequests,
}

impl std::fmt::Display for HttpClientError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::SerializeError(error) => write!(f, "failed to serialize API request: {error}"),
            Self::ParseError(error, status, _) => {
                write!(f, "failed to parse API response ({status}): {error}")
            }
            Self::RequestError(error) => write!(f, "API request failed: {error}"),
            Self::TooManyRequests => write!(f, "API rate limit exceeded"),
        }
    }
}

impl std::error::Error for HttpClientError {}

impl From<reqwest::Error> for HttpClientError {
    fn from(value: reqwest::Error) -> Self {
        HttpClientError::RequestError(value)
    }
}
