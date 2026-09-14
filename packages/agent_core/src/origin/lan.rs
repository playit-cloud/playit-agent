//! Local sockets toward origin services.
//!
//! When the origin listens on IPv4 loopback the agent binds each client to its own
//! `127.x.y.z` address derived from the client's public address. Origins that log or
//! rate-limit by peer address then see distinct clients instead of one `127.0.0.1`.

use std::{
    io,
    net::{IpAddr, Ipv4Addr, Ipv6Addr, SocketAddr, SocketAddrV4},
};

use byteorder::{BigEndian, ByteOrder};
use tokio::net::{TcpSocket, TcpStream, UdpSocket};

/// Connects to `target`. If `client_loopback_ip` is set and the target is IPv4
/// loopback, the connection is bound to a per-client loopback address first and falls
/// back to a normal connect if that is not possible.
pub async fn connect_tcp(
    client_loopback_ip: bool,
    peer: SocketAddr,
    target: SocketAddr,
) -> io::Result<TcpStream> {
    if client_loopback_ip && is_ip4_loopback(target) {
        match connect_tcp_from_loopback(peer, target).await {
            Ok(stream) => return Ok(stream),
            Err(error) => tracing::debug!(
                ?error,
                %peer,
                %target,
                "per-client loopback bind failed, connecting normally"
            ),
        }
    }

    TcpStream::connect(target).await
}

async fn connect_tcp_from_loopback(peer: SocketAddr, target: SocketAddr) -> io::Result<TcpStream> {
    let socket = TcpSocket::new_v4()?;
    socket.bind(SocketAddrV4::new(client_loopback_ip4(peer.ip()), 0).into())?;
    socket.connect(target).await
}

/// Binds a socket to talk to `target` on behalf of `peer`.
///
/// The local port is derived from the peer and tunnel so a returning client lands on
/// the same local port when possible. Bind failures fall back to any free port and
/// then to the unspecified address.
pub async fn bind_udp(
    client_loopback_ip: bool,
    peer: SocketAddr,
    target: SocketAddr,
    tunnel_id: u64,
) -> io::Result<UdpSocket> {
    let ip_hash = hash_ip(peer.ip());
    let port_hash =
        mix(peer.port() as u32) ^ mix((tunnel_id >> 32) as u32) ^ mix(tunnel_id as u32) ^ ip_hash;
    let preferred_port = (2048u32 + port_hash % (u16::MAX as u32 - 2048u32)) as u16;

    if client_loopback_ip && is_ip4_loopback(target) {
        let local_ip = Ipv4Addr::from(loopback_masked(ip_hash));

        match UdpSocket::bind(SocketAddrV4::new(local_ip, preferred_port)).await {
            Ok(socket) => return Ok(socket),
            Err(error) => tracing::debug!(?error, preferred_port, "preferred udp port unavailable"),
        }
        match UdpSocket::bind(SocketAddrV4::new(local_ip, 0)).await {
            Ok(socket) => return Ok(socket),
            Err(error) => tracing::debug!(?error, %local_ip, "per-client loopback bind failed"),
        }
    }

    let unspecified = match target {
        SocketAddr::V4(_) => IpAddr::V4(Ipv4Addr::UNSPECIFIED),
        SocketAddr::V6(_) => IpAddr::V6(Ipv6Addr::UNSPECIFIED),
    };

    match UdpSocket::bind(SocketAddr::new(unspecified, preferred_port)).await {
        Ok(socket) => Ok(socket),
        Err(error) => {
            tracing::debug!(?error, preferred_port, "preferred udp port unavailable");
            UdpSocket::bind(SocketAddr::new(unspecified, 0)).await
        }
    }
}

fn is_ip4_loopback(addr: SocketAddr) -> bool {
    matches!(addr, SocketAddr::V4(v4) if v4.ip().is_loopback())
}

fn client_loopback_ip4(ip: IpAddr) -> Ipv4Addr {
    Ipv4Addr::from(loopback_masked(hash_ip(ip)))
}

/// Maps a hash into `127.0.0.1`..`127.255.255.255`.
fn loopback_masked(hash: u32) -> u32 {
    let host = mix(hash) & 0x00FF_FFFF;
    0x7F00_0000 | host.max(1)
}

fn hash_ip(ip: IpAddr) -> u32 {
    match ip {
        IpAddr::V4(ip) => u32::from(ip),
        IpAddr::V6(ip) => {
            let bytes = ip.octets();
            mix(BigEndian::read_u32(&bytes[..4]))
                ^ mix(BigEndian::read_u32(&bytes[4..8]))
                ^ mix(BigEndian::read_u32(&bytes[8..12]))
                ^ mix(BigEndian::read_u32(&bytes[12..16]))
        }
    }
}

/// Integer hash (lowbias32 style) used to spread peers across ports and addresses.
fn mix(mut v: u32) -> u32 {
    v = ((v >> 16) ^ v).wrapping_mul(0x45d9f3);
    v = ((v >> 16) ^ v).wrapping_mul(0x45d9f3);
    (v >> 16) ^ v
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn loopback_addresses_stay_in_127_range() {
        for seed in [0u32, 1, 0xFFFF_FFFF, 0x7F00_0001, 12345] {
            let ip = Ipv4Addr::from(loopback_masked(seed));
            assert!(ip.is_loopback(), "{ip}");
            assert_ne!(ip, Ipv4Addr::new(127, 0, 0, 0));
        }
    }

    #[test]
    fn same_peer_gets_same_loopback_ip() {
        let peer: IpAddr = "203.0.113.9".parse().unwrap();
        assert_eq!(client_loopback_ip4(peer), client_loopback_ip4(peer));
        let other: IpAddr = "203.0.113.10".parse().unwrap();
        assert_ne!(client_loopback_ip4(peer), client_loopback_ip4(other));
    }
}
