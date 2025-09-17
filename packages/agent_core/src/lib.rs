extern crate core;

pub mod agent_control;
pub mod network;
pub mod playit_agent;
pub mod utils;

pub const PROTOCOL_VERSION: u64 = 2;

#[cfg(test)]
mod test {
    #[test]
    fn test() {}
}
