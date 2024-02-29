use std::convert::Infallible;

use async_trait::async_trait;
use bytes::{Buf, Bytes};
use http_body_util::{combinators::BoxBody, BodyExt, Full};
use hyper::{header, Method, Request, StatusCode};
use hyper_util::{client::legacy::{connect::HttpConnector, Client}, rt::TokioExecutor};
use serde::de::DeserializeOwned;
use serde::Serialize;

use crate::api::api::{ApiResult, PlayitHttpClient};

pub struct HttpClient {
    api_base: String,
    auth_header: Option<String>,

    client: Client<hyper_rustls::HttpsConnector<HttpConnector>, BoxBody<Bytes, Infallible>>,
}

impl HttpClient {
    pub fn new(api_base: String, auth_header: Option<String>) -> Self {
        let connector = if api_base.starts_with("http://") {
            hyper_rustls::HttpsConnectorBuilder::new()
                .with_webpki_roots()
                .https_or_http()
                .enable_http1()
                .enable_http2()
                .build()
        } else {
            hyper_rustls::HttpsConnectorBuilder::new()
                .with_webpki_roots()
                .https_only()
                .enable_http1()
                .enable_http2()
                .build()
        };

        HttpClient {
            api_base,
            auth_header,
            client: Client::builder(TokioExecutor::new()).build(connector),
        }
    }

    pub fn api_base(&self) -> &str {
        &self.api_base
    }
}

#[async_trait]
impl PlayitHttpClient for HttpClient {
    type Error = HttpClientError;

    async fn call<Req: Serialize + Send, Res: DeserializeOwned, Err: DeserializeOwned>(&self, path: &str, req: Req) -> Result<ApiResult<Res, Err>, Self::Error> {
        let mut builder = Request::builder()
            .uri(format!("{}{}", self.api_base, path))
            .method(Method::POST);

        if let Some(auth_header) = &self.auth_header {
            builder = builder.header(
                header::AUTHORIZATION,
                auth_header,
            );
        }

        let res = async move {
            let request_str = serde_json::to_string(&req)
                .map_err(|e| HttpClientError::SerializeError(e))?;

            let request = builder
                .body(BoxBody::new(Full::new(Bytes::from(request_str))))
                .unwrap();

            let response = self.client.request(request).await
                .map_err(|e| HttpClientError::RequestError(e))?;

            let response_status = response.status();
            if response_status == StatusCode::TOO_MANY_REQUESTS {
                return Err(HttpClientError::TooManyRequests);
            }

            let bytes = response.into_body().collect().await
                .map_err(|e| HttpClientError::BodyReadError(e))?
                .aggregate();

            let response_txt = String::from_utf8_lossy(bytes.chunk());

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
    RequestError(hyper_util::client::legacy::Error),
    BodyReadError(hyper::Error),
    TooManyRequests,
}