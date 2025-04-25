use std::panic::Location;

use reqwest::StatusCode;
use serde::de::DeserializeOwned;
use serde::Serialize;
use tokio::sync::RwLock;

use crate::api::{ApiResult, PlayitHttpClient};

pub struct HttpClient {
    api_base: String,
    auth_header: RwLock<Option<String>>,
    client: reqwest::Client,
}

impl Clone for HttpClient {
    fn clone(&self) -> Self {
        Self {
            api_base: self.api_base.clone(),
            auth_header: match self.auth_header.try_read() {
                Ok(v) => RwLock::new(v.clone()),
                _ => RwLock::new(None),
            },
            client: self.client.clone()
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

    async fn call<Req: Serialize + Send, Res: DeserializeOwned, Err: DeserializeOwned>(&self, _caller: &'static Location<'static>, path: &str, req: Req) -> Result<ApiResult<Res, Err>, Self::Error> {
        let mut builder = self.client.post(format!("{}{}", self.api_base, path));

        {
            let lock = self.auth_header.read().await;

            if let Some(auth_header) = &*lock {
                builder = builder.header(
                    reqwest::header::AUTHORIZATION,
                    auth_header,
                );
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
            let result: ApiResult<Res, Err> = serde_json::from_str(&response_txt)
                .map_err(|e| {
                    tracing::error!("failed to parse json:\n{}", response_txt);
                    HttpClientError::ParseError(e, response_status, response_txt.to_string())
                })?;

            Ok::<_, Self::Error>(result)
        }.await;

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

impl From<reqwest::Error> for HttpClientError {
    fn from(value: reqwest::Error) -> Self {
        HttpClientError::RequestError(value)
    }

}
