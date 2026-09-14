/// Milliseconds since the unix epoch. The control protocol exchanges wall-clock
/// timestamps with the tunnel server, so this is the only place they come from.
pub fn now_milli() -> u64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_millis() as u64)
        .unwrap_or(0)
}
