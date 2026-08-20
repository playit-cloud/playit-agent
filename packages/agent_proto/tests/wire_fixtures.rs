use std::net::{Ipv6Addr, SocketAddrV4};
use std::num::{NonZeroU16, NonZeroU64};

use playit_agent_proto::udp_proto::{FragmentInfo, UdpFlow, UdpFlowExtension};

fn fixtures() -> Vec<(&'static str, UdpFlow)> {
    let extension = UdpFlowExtension {
        client_server_id: NonZeroU64::new(12).unwrap(),
        tunnel_id: NonZeroU64::new(123).unwrap(),
        port_offset: 9,
    };

    vec![
        (
            "v4_legacy",
            UdpFlow::V4 {
                src: SocketAddrV4::new([192, 0, 2, 10].into(), 25565),
                dst: SocketAddrV4::new([127, 0, 0, 1].into(), 19132),
                frag: None,
                extension: None,
            },
        ),
        (
            "v4_extension",
            UdpFlow::V4 {
                src: SocketAddrV4::new([192, 0, 2, 10].into(), 25565),
                dst: SocketAddrV4::new([127, 0, 0, 1].into(), 19132),
                frag: None,
                extension: Some(extension),
            },
        ),
        (
            "v4_fragment",
            UdpFlow::V4 {
                src: SocketAddrV4::new([192, 0, 2, 10].into(), 25565),
                dst: SocketAddrV4::new([127, 0, 0, 1].into(), 19132),
                frag: Some(FragmentInfo {
                    packet_id: NonZeroU16::new(513).unwrap(),
                    frag_offset: 7,
                    has_more: true,
                }),
                extension: Some(extension),
            },
        ),
        (
            "v6_legacy",
            UdpFlow::V6 {
                src: ("2001:db8::10".parse::<Ipv6Addr>().unwrap(), 25565),
                dst: ("::1".parse::<Ipv6Addr>().unwrap(), 19132),
                extension: None,
            },
        ),
        (
            "v6_extension",
            UdpFlow::V6 {
                src: ("2001:db8::10".parse::<Ipv6Addr>().unwrap(), 25565),
                dst: ("::1".parse::<Ipv6Addr>().unwrap(), 19132),
                extension: Some(extension),
            },
        ),
    ]
}

#[test]
fn udp_flow_bytes_remain_wire_compatible() {
    let expected: std::collections::BTreeMap<_, _> = include_str!("../fixtures/udp_flow.hex")
        .lines()
        .map(|line| line.split_once('=').unwrap())
        .collect();

    for (name, flow) in fixtures() {
        let mut bytes = vec![0; flow.footer_len()];
        assert!(flow.write_to(&mut bytes));
        assert_eq!(hex::encode(&bytes), expected[name], "fixture {name}");
        assert_eq!(UdpFlow::from_tail(&bytes).unwrap(), flow, "fixture {name}");
    }
}
