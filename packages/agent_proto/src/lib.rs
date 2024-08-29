use std::io::{Error, ErrorKind, Read, Write};
use std::net::IpAddr;

use byteorder::{BigEndian, ReadBytesExt, WriteBytesExt};
use message_encoding::MessageEncoding;
use serde::{Deserialize, Serialize};

pub mod control_messages;
pub mod control_feed;
pub mod rpc;
pub mod hmac;
pub mod raw_slice;

#[derive(Serialize, Deserialize, Clone, Eq, PartialEq, Debug, Hash)]
pub struct AgentSessionId {
    pub session_id: u64,
    pub account_id: u64,
    pub agent_id: u64,
}

#[derive(Eq, PartialEq, Ord, PartialOrd, Debug, Serialize, Deserialize, Hash, Clone)]
pub struct PortRange {
    pub ip: IpAddr,
    pub port_start: u16,
    pub port_end: u16,
    pub port_proto: PortProto,
}

#[derive(Copy, Clone, Eq, PartialEq, Ord, PartialOrd, Debug, Serialize, Hash, Deserialize)]
pub enum PortProto {
    #[serde(rename = "both")]
    Both,
    #[serde(rename = "tcp")]
    Tcp,
    #[serde(rename = "udp")]
    Udp,
}

impl MessageEncoding for AgentSessionId {
    const STATIC_SIZE: Option<usize> = Some(8 * 3);

    fn write_to<T: Write>(&self, out: &mut T) -> std::io::Result<usize> {
        let mut sum = 0;
        sum += self.session_id.write_to(out)?;
        sum += self.account_id.write_to(out)?;
        sum += self.agent_id.write_to(out)?;
        Ok(sum)
    }

    fn read_from<T: Read>(read: &mut T) -> std::io::Result<Self> {
        Ok(AgentSessionId {
            session_id: read.read_u64::<BigEndian>()?,
            account_id: read.read_u64::<BigEndian>()?,
            agent_id: read.read_u64::<BigEndian>()?,
        })
    }
}

impl MessageEncoding for PortRange {
    const MAX_SIZE: Option<usize> = Some(
        match IpAddr::MAX_SIZE { Some(v) => v, _ => panic!() }
        + 4
        + match PortProto::MAX_SIZE { Some(v) => v, _ => panic!() }
    );

    fn write_to<T: Write>(&self, out: &mut T) -> std::io::Result<usize> {
        let mut len = 4;
        len += self.ip.write_to(out)?;
        out.write_u16::<BigEndian>(self.port_start)?;
        out.write_u16::<BigEndian>(self.port_end)?;
        len += self.port_proto.write_to(out)?;
        Ok(len)
    }

    fn read_from<T: Read>(read: &mut T) -> std::io::Result<Self> {
        Ok(PortRange {
            ip: IpAddr::read_from(read)?,
            port_start: read.read_u16::<BigEndian>()?,
            port_end: read.read_u16::<BigEndian>()?,
            port_proto: PortProto::read_from(read)?,
        })
    }
}

impl MessageEncoding for PortProto {
    const STATIC_SIZE: Option<usize> = Some(1);

    fn write_to<T: Write>(&self, out: &mut T) -> std::io::Result<usize> {
        match self {
            PortProto::Tcp => out.write_u8(1),
            PortProto::Udp => out.write_u8(2),
            PortProto::Both => out.write_u8(3),
        }?;

        Ok(1)
    }

    fn read_from<T: Read>(read: &mut T) -> std::io::Result<Self> {
        match read.read_u8()? {
            1 => Ok(PortProto::Tcp),
            2 => Ok(PortProto::Udp),
            3 => Ok(PortProto::Both),
            _ => Err(Error::new(ErrorKind::Other, "invalid port proto")),
        }
    }
}
