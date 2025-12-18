use std::sync::atomic::{AtomicU64, Ordering};

use serde::Serialize;

#[derive(Default, Debug)]
pub struct IntCounter(AtomicU64);

impl Serialize for IntCounter {
    fn serialize<S>(&self, serializer: S) -> Result<S::Ok, S::Error>
    where
        S: serde::Serializer,
    {
        serializer.serialize_u64(self.0.load(Ordering::Acquire))
    }
}

impl IntCounter {
    pub fn inc(&self) {
        self.0.fetch_add(1, Ordering::AcqRel);
    }
}
