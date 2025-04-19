use std::sync::{atomic::{AtomicU64, Ordering}, Arc, LazyLock};

use serde::Serialize;

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
    pub origin_reject_addr_differ: IntCounter,
    pub origin_reject_port_too_low: IntCounter,
    pub origin_reject_port_too_high: IntCounter,
}

#[derive(Default, Debug)]
pub struct IntCounter(AtomicU64);

impl Serialize for IntCounter {
    fn serialize<S>(&self, serializer: S) -> Result<S::Ok, S::Error> where S: serde::Serializer {
        serializer.serialize_u64(self.0.load(Ordering::Acquire))
    }
}

impl IntCounter {
    pub fn inc(&self) {
        self.0.fetch_add(1, Ordering::AcqRel);
    }
}

pub fn udp_errors() -> &'static UdpErrors {
    &_ERRORS
}
