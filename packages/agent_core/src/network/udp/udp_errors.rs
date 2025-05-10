use std::sync::{Arc, LazyLock};

use serde::Serialize;

use crate::network::errors::IntCounter;

static _ERRORS: LazyLock<Arc<UdpErrors>> = LazyLock::new(|| Arc::new(UdpErrors::default()));

#[derive(Default, Debug, Serialize)]
pub struct UdpErrors {
    pub no_session_send_fail: IntCounter,
    pub tail_append_fail: IntCounter,
    pub send_io_error: IntCounter,
    pub recv_io_error: IntCounter,
    pub recv_with_no_session: IntCounter,
    pub recv_source_no_match: IntCounter,
    pub recv_too_small: IntCounter,
    pub recv_invalid_footer_id: IntCounter,
    pub establish_send_io_error: IntCounter,
    pub establish_no_session: IntCounter,

    pub new_client_ratelimit: IntCounter,
    pub origin_client_missing: IntCounter,
    pub origin_reject_bad_id: IntCounter,
    pub origin_tunnel_not_found: IntCounter,
    pub origin_source_not_ip4: IntCounter,
    pub origin_reject_addr_differ: IntCounter,
    pub origin_reject_port_too_low: IntCounter,
    pub origin_reject_port_too_high: IntCounter,
    pub origin_send_io_error: IntCounter,
    pub origin_v1_proxy_protocol: IntCounter,
}

pub fn udp_errors() -> &'static UdpErrors {
    &_ERRORS
}
