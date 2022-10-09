use playit_agent_core::network::udp_clients::UdpClients;
use playit_agent_core::tunnel::simple_tunnel::SimpleTunnel;

pub struct TunnelRun {
    tunnel: SimpleTunnel,
    udp_clients: UdpClients<>
}

impl TunnelRun {
    pub async fn start(self) {

    }
}