use std::net::SocketAddr;

use tokio::io::{AsyncReadExt, AsyncWriteExt};
use tokio::net::TcpStream;

use playit_agent_proto::control_feed::{ClaimInstructions};

use crate::network::lan_address::LanAddress;

pub struct TcpTunnel {
    claim_instruction: ClaimInstructions,
    peer_addr: SocketAddr,
    pub use_special_lan: bool,
}

impl TcpTunnel {
    pub fn new(claim_instruction: ClaimInstructions, peer_addr: SocketAddr) -> Self {
        TcpTunnel { claim_instruction, use_special_lan: true, peer_addr }
    }

    pub async fn connect(self) -> std::io::Result<TcpStream> {
        let mut stream = LanAddress::tcp_socket(
            self.use_special_lan,
            self.peer_addr,
            self.claim_instruction.address,
        ).await?;

        stream.write_all(&self.claim_instruction.token).await?;

        let mut response = [0u8; 8];
        stream.read_exact(&mut response).await?;

        Ok(stream)
    }
}