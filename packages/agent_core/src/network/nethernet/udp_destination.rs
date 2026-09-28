//! Picks where a NetherNet session's UDP goes: the Bedrock server's IP at the
//! port from the SDP answer. The server never binds 127.0.0.1, so a loopback
//! origin is swapped for a real interface on this machine.

use std::net::{IpAddr, SocketAddr};

/// `public` is only used to pick the interface when `origin_ip` is loopback.
pub fn get_bedrock_udp_addr(
    origin_ip: IpAddr,
    port: u16,
    public: SocketAddr,
) -> std::io::Result<SocketAddr> {
    let origin_ip = origin_ip.to_canonical();
    let ip = if origin_ip.is_loopback() {
        get_sending_interface(public)?
    } else {
        origin_ip
    };
    Ok(SocketAddr::new(ip, port))
}

/// The interface this machine would send to `target` from.
fn get_sending_interface(target: SocketAddr) -> std::io::Result<IpAddr> {
    let unspecified: SocketAddr = if target.is_ipv4() {
        (std::net::Ipv4Addr::UNSPECIFIED, 0).into()
    } else {
        (std::net::Ipv6Addr::UNSPECIFIED, 0).into()
    };
    let socket = std::net::UdpSocket::bind(unspecified)?;
    socket.connect(target)?;
    Ok(socket.local_addr()?.ip())
}

#[cfg(test)]
mod test {
    use std::net::Ipv4Addr;

    use super::*;

    const PUBLIC: SocketAddr = SocketAddr::new(IpAddr::V4(Ipv4Addr::new(147, 185, 221, 2)), 30123);

    #[test]
    fn origin_keeps_its_address_unless_it_is_loopback() {
        let lan = IpAddr::V4(Ipv4Addr::new(192, 168, 1, 11));
        assert_eq!(
            get_bedrock_udp_addr(lan, 19140, PUBLIC).unwrap(),
            SocketAddr::new(lan, 19140)
        );
        assert_eq!(
            get_bedrock_udp_addr("::ffff:192.168.1.11".parse().unwrap(), 19140, PUBLIC).unwrap(),
            SocketAddr::new(lan, 19140)
        );

        /* Any loopback spelling resolves to a real interface, or fails when there is no route. */
        for origin in ["127.0.0.1", "::1", "::ffff:127.0.0.1"] {
            if let Ok(resolved) = get_bedrock_udp_addr(origin.parse().unwrap(), 19140, PUBLIC) {
                assert!(!resolved.ip().is_loopback(), "{origin} stayed loopback");
                assert!(!resolved.ip().is_unspecified());
                assert_eq!(resolved.port(), 19140);
            }
        }
    }
}
