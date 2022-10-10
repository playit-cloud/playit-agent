use std::net::{Ipv4Addr, Ipv6Addr, SocketAddr, SocketAddrV4};
use std::ops::Add;
use std::sync::Arc;
use std::time::Duration;
use tokio::net::TcpStream;

use tracing::Instrument;

use crate::network::address_lookup::{AddressLookup, MatchAddress};
use crate::network::tcp_clients::TcpClients;
use crate::network::udp_clients::UdpClients;
use crate::tunnel::setup::SetupError;
use crate::tunnel::simple_tunnel::SimpleTunnel;
use crate::tunnel::udp_tunnel::UdpTunnelRx;
use playit_agent_proto::PortProto;
use crate::network::tcp_pipe::pipe;

pub struct TunnelRunner<L: AddressLookup> {
    lookup: Arc<L>,
    tunnel: SimpleTunnel,
    udp_clients: UdpClients<Arc<L>>,
    tcp_clients: TcpClients,
}

impl<L: AddressLookup + Sync + Send> TunnelRunner<L> {
    pub async fn new(secret_key: String, lookup: Arc<L>) -> Result<Self, SetupError> {
        let tunnel = SimpleTunnel::setup(secret_key).await?;
        let udp_clients = UdpClients::new(tunnel.udp_tunnel(), lookup.clone());

        Ok(TunnelRunner {
            lookup,
            tunnel,
            udp_clients,
            tcp_clients: TcpClients::new(),
        })
    }

    pub async fn run(mut self) {
        let mut tunnel = self.tunnel;
        let mut udp = tunnel.udp_tunnel();

        let tunnel_task = tokio::spawn(async move {
            loop {
                if let Some(new_client) = tunnel.update().await {
                    let clients = self.tcp_clients.clone();
                    let span = tracing::info_span!("tcp client", ?new_client);

                    let local_addr = match self.lookup.tunnel_match_address(new_client.connect_addr).and_then(|addr| self.lookup.local_address(addr, PortProto::Tcp)) {
                        Some(addr) => addr,
                        None => {
                            tracing::info!("could not find local address for connection");
                            continue;
                        }
                    };

                    tokio::spawn(async move {
                        let tunnel_conn = match clients.connect(new_client.clone()).await {
                            Ok(Some(client)) => client,
                            Ok(None) => return,
                            Err(error) => {
                                tracing::error!(?error, "failed to accept new client");
                                return;
                            }
                        };

                        tracing::info!("connected to TCP tunnel");

                        let local_conn = match TcpStream::connect(local_addr).await {
                            Ok(v) => v,
                            Err(error) => {
                                tracing::error!(?error, "failed to connect to local server");
                                return;
                            }
                        };

                        let (tunnel_read, tunnel_write) = tunnel_conn.into_split();
                        let (local_read, local_write) = local_conn.into_split();

                        tokio::spawn(pipe(tunnel_read, local_write));
                        tokio::spawn(pipe(local_read, tunnel_write));
                    }.instrument(span));
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