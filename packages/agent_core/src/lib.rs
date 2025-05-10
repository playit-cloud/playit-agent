extern crate core;

pub mod agent_control;
pub mod network;
pub mod utils;
pub mod playit_agent;

pub const PROTOCOL_VERSION: u64 = 2;

#[cfg(test)]
mod test {
    #[test]
    fn test() {
    }
}
