use tokio::io::{AsyncReadExt, AsyncWriteExt};
use tokio::net::TcpStream;

use playit_agent_proto::control_feed::ClaimInstructions;

pub struct TcpTunnel {
    claim_instruction: ClaimInstructions,
}

impl TcpTunnel {
    pub fn new(claim_instruction: ClaimInstructions) -> Self {
        TcpTunnel { claim_instruction }
    }

    pub async fn connect(self) -> std::io::Result<TcpStream> {
        let mut stream = match TcpStream::connect(self.claim_instruction.address).await {
            Ok(v) => v,
            Err(error) => {
                tracing::error!(addr = %self.claim_instruction.address, ?error, "Failed to establish connection to tunnel server");
                return Err(error)
            }
        };

        stream.write_all(&self.claim_instruction.token).await?;

        let mut response = [0u8; 8];
        stream.read_exact(&mut response).await?;

        Ok(stream)
    }
}

