use std::io::{Error, ErrorKind, Read, Write};
use std::net::IpAddr;

use byteorder::{BigEndian, ReadBytesExt, WriteBytesExt};
use serde::{Deserialize, Serialize};

use crate::encoding::MessageEncoding;

pub mod control_messages;
pub mod control_feed;
pub mod encoding;
pub mod rpc;
pub mod hmac;
pub mod raw_slice;

#[derive(Serialize, Deserialize, Clone, Eq, PartialEq, Debug)]
pub struct AgentSessionId {
    pub session_id: u64,
    pub account_id: u64,
    pub agent_id: u64,
}

#[derive(Eq, PartialEq, Ord, PartialOrd, Debug, Serialize, Deserialize, Clone)]
pub struct PortRange {
    pub ip: IpAddr,
    pub port_start: u16,
    pub port_end: u16,
    pub port_proto: PortProto,
}

#[derive(Copy, Clone, Eq, PartialEq, Ord, PartialOrd, Debug, Serialize, Deserialize)]
pub enum PortProto {
    Both,
    Tcp,
    Udp,
}

impl MessageEncoding for AgentSessionId {
    fn write_to<T: Write>(&self, out: &mut T) -> std::io::Result<()> {
        out.write_u64::<BigEndian>(self.session_id)?;
        out.write_u64::<BigEndian>(self.account_id)?;
        out.write_u64::<BigEndian>(self.agent_id)?;
        Ok(())
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
    fn write_to<T: Write>(&self, mut out: &mut T) -> std::io::Result<()> {
        self.ip.write_to(out)?;
        out.write_u16::<BigEndian>(self.port_start)?;
        out.write_u16::<BigEndian>(self.port_end)?;
        self.port_proto.write_to(out)
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
    fn write_to<T: Write>(&self, out: &mut T) -> std::io::Result<()> {
        match self {
            PortProto::Tcp => out.write_u8(1),
            PortProto::Udp => out.write_u8(2),
            PortProto::Both => out.write_u8(3),
        }
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