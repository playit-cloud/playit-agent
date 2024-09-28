pub mod name_lookup;
pub mod error_helper;
pub mod shuffle;
pub mod key_to_id;
pub mod instance_count;
pub mod id_slab;
pub mod non_overlapping;
pub mod ip_bytes;

pub fn now_milli() -> u64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .unwrap()
        .as_millis() as u64
}

pub fn now_sec() -> u32 {
    (now_milli() / 1_000) as u32
}