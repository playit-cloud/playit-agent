use std::net::{IpAddr, SocketAddr};

pub trait LocalResolver {
    fn resolve(&self, tunnel_addr: SocketAddr, proto: Proto) -> Option<SocketAddr>;
}

pub enum Proto {
    Tcp,
    Udp,
}