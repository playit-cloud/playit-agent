use std::net::{Ipv4Addr, Ipv6Addr, SocketAddr, SocketAddrV4};
use std::time::Duration;
use playit_agent_core::network::address_lookup::{AddressLookup, MatchAddress};
use playit_agent_core::network::udp_clients::UdpClients;
use playit_agent_core::tunnel::setup::SetupError;
use playit_agent_core::tunnel::simple_tunnel::SimpleTunnel;
use playit_agent_core::tunnel::udp_tunnel::UdpTunnelRx;
use playit_agent_proto::PortProto;

pub struct TunnelRun {
    tunnel: SimpleTunnel,
    udp_clients: UdpClients<SimpleLookup>
}

struct SimpleLookup;

impl AddressLookup for SimpleLookup {
    fn find_tunnel_port_range(&self, match_ip: Ipv6Addr, port: u16) -> Option<(u16, u16)> {
        Some((port, port + 1))
    }

    fn local_address(&self, match_addr: MatchAddress, proto: PortProto) -> Option<SocketAddr> {
        Some(SocketAddr::V4(SocketAddrV4::new(Ipv4Addr::LOCALHOST, match_addr.from_port)))
    }
}

impl TunnelRun {
    pub async fn new(secret_key: String) -> Result<Self, SetupError> {
        let tunnel = SimpleTunnel::setup(secret_key).await?;
        let udp_clients = UdpClients::new(tunnel.udp_tunnel(), SimpleLookup);

        Ok(TunnelRun {
            tunnel,
            udp_clients
        })
    }

    pub async fn run(mut self) {
        let mut tunnel = self.tunnel;
        let mut udp = tunnel.udp_tunnel();

        let tunnel_task = tokio::spawn(async move {
            loop {
                if let Some(client) = tunnel.update().await {
                    tracing::info!(?client, "got new tcp client");
                }
            }
        });

        let mut udp_clients = self.udp_clients;
        let udp_task = tokio::spawn(async move {
            let mut buffer = vec![0u8; 2048];
            loop {
                let rx = match udp.receive_from(&mut buffer).await {
                    Ok(v) => v,
                    Err(error) => {
                        tracing::error!(?error, "got error");
                        tokio::time::sleep(Duration::from_secs(1)).await;
                        continue;
                    }
                };

                match rx {
                    UdpTunnelRx::ReceivedPacket { bytes, flow } => {
                        tracing::info!(bytes, ?flow, "got packet");
                        udp_clients.forward_packet(&flow, &buffer[..bytes]).await.unwrap();
                    }
                    UdpTunnelRx::ConfirmedConnection => {}
                }
            }
        });

        tunnel_task.await.unwrap();
        udp_task.await.unwrap();
    }
}