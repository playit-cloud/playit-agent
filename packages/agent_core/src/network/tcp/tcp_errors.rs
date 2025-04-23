use std::sync::{Arc, LazyLock};

use serde::Serialize;

use crate::network::errors::IntCounter;

static _ERRORS: LazyLock<Arc<TcpErrors>> = LazyLock::new(|| Arc::new(TcpErrors::default()));

#[derive(Default, Debug, Serialize)]
pub struct TcpErrors {
    pub new_client_origin_not_found: IntCounter,
    pub new_client_rate_limited: IntCounter,
    pub new_client_invalid_port_offset: IntCounter,
    pub new_client_claim_connect_timeout: IntCounter,
    pub new_client_claim_connect_error: IntCounter,
    pub new_client_send_claim_timeout: IntCounter,
    pub new_client_send_claim_error: IntCounter,
}

pub fn tcp_errors() -> &'static TcpErrors {
    &_ERRORS
}
