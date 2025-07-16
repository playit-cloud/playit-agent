use std::fmt::Debug;
use std::io::{Read, Write};
use std::net::SocketAddr;
use std::ops::Not;
use std::sync::Arc;

use byteorder::{BigEndian, ReadBytesExt, WriteBytesExt};
use message_encoding::{m_max, m_max_list, m_opt_sum, m_static, MessageEncoding};
use serde::ser::SerializeStruct;
use serde::Serialize;

use crate::{AgentSessionId, PortRange};
use crate::hmac::HmacSha256;

#[derive(Debug, Eq, PartialEq, Clone, Serialize)]
pub enum ControlRequest {
    Ping(Ping),
    AgentRegister(AgentRegister),
    AgentKeepAlive(AgentSessionId),
    SetupUdpChannel(AgentSessionId),
    AgentCheckPortMapping(AgentCheckPortMapping),
}

#[repr(u32)]
#[derive(Copy, Clone, PartialEq, Eq, Hash)]
pub enum ControlRequestId {
    _PingV1 = 1,
    AgentRegisterV1,
    AgentKeepAliveV1,
    SetupUdpChannelV1,
    AgentCheckPortMappingV1,
    PingV2,
    AgentRegisterV2,
    END,
}

impl ControlRequestId {
    pub fn from_num(num: u32) -> Option<Self> {
        if (Self::END as u32) <= num || num == 0 {
            return None;
        }
        Some(unsafe { std::mem::transmute::<u32, Self>(num) })
    }
}

impl MessageEncoding for ControlRequestId {
    const STATIC_SIZE: Option<usize> = Some(4);
    
    fn write_to<T: Write>(&self, out: &mut T) -> std::io::Result<usize> {
        (*self as u32).write_to(out)
    }

    fn read_from<T: Read>(read: &mut T) -> std::io::Result<Self> {
        let v = u32::read_from(read)?;
        ControlRequestId::from_num(v)
            .ok_or_else(|| std::io::Error::new(std::io::ErrorKind::InvalidData, "invalid request id"))
    }
}

impl MessageEncoding for ControlRequest {
    const MAX_SIZE: Option<usize> = Some(m_static::<ControlRequestId>() + m_max_list(&[
        m_max::<Ping>(),
        m_max::<AgentRegister>(),
        m_max::<AgentSessionId>(),
        m_max::<AgentCheckPortMapping>(),
    ]));

    fn write_to<T: Write>(&self, out: &mut T) -> std::io::Result<usize> {
        let mut sum = 0;

        match self {
            ControlRequest::Ping(data) => {
                sum += ControlRequestId::PingV2.write_to(out)?;
                sum += data.write_to(out)?;
            }
            ControlRequest::AgentRegister(data) => {
                if data.proto_version <= 1 {
                    sum += ControlRequestId::AgentRegisterV1.write_to(out)?;
                } else {
                    sum += ControlRequestId::AgentRegisterV2.write_to(out)?;
                }
                sum += data.write_to(out)?;
            }
            ControlRequest::AgentKeepAlive(data) => {
                sum += ControlRequestId::AgentKeepAliveV1.write_to(out)?;
                sum += data.write_to(out)?;
            }
            ControlRequest::SetupUdpChannel(data) => {
                sum += ControlRequestId::SetupUdpChannelV1.write_to(out)?;
                sum += data.write_to(out)?;
            }
            ControlRequest::AgentCheckPortMapping(data) => {
                sum += ControlRequestId::AgentCheckPortMappingV1.write_to(out)?;
                sum += data.write_to(out)?;
            }
        }

        Ok(sum)
    }

    fn read_from<T: Read>(read: &mut T) -> std::io::Result<Self> {
        let id = ControlRequestId::read_from(read)?;
        
        match id {
            ControlRequestId::PingV2 => Ok(ControlRequest::Ping(Ping::read_from(read)?)),
            ControlRequestId::AgentRegisterV1 => Ok(ControlRequest::AgentRegister(AgentRegisterV1::read_from(read)?.upgrade())),
            ControlRequestId::AgentRegisterV2 => Ok(ControlRequest::AgentRegister(AgentRegister::read_from(read)?)),
            ControlRequestId::AgentKeepAliveV1 => Ok(ControlRequest::AgentKeepAlive(AgentSessionId::read_from(read)?)),
            ControlRequestId::SetupUdpChannelV1 => Ok(ControlRequest::SetupUdpChannel(AgentSessionId::read_from(read)?)),
            ControlRequestId::AgentCheckPortMappingV1 => Ok(ControlRequest::AgentCheckPortMapping(AgentCheckPortMapping::read_from(read)?)),
            ControlRequestId::_PingV1 => Ok(ControlRequest::Ping(Ping {
                now: u64::read_from(read)?,
                session_id: None,
                current_ping: None,
            })),
            _ => Err(std::io::Error::other("old control request no longer supported")),
        }
    }
}

#[derive(Debug, Eq, PartialEq, Clone, Serialize)]
pub struct AgentCheckPortMapping {
    pub agent_session_id: AgentSessionId,
    pub port_range: PortRange,
}

impl MessageEncoding for AgentCheckPortMapping {
    const MAX_SIZE: Option<usize> = Some(m_static::<AgentSessionId>() + m_max::<PortRange>());

    fn write_to<T: Write>(&self, out: &mut T) -> std::io::Result<usize> {
        let mut sum = 0;
        sum += self.agent_session_id.write_to(out)?;
        sum += self.port_range.write_to(out)?;
        Ok(sum)
    }

    fn read_from<T: Read>(read: &mut T) -> std::io::Result<Self> {
        Ok(AgentCheckPortMapping {
            agent_session_id: AgentSessionId::read_from(read)?,
            port_range: PortRange::read_from(read)?,
        })
    }
}

#[derive(Debug, Eq, PartialEq, Clone, Serialize)]
pub struct Ping {
    pub now: u64,
    pub current_ping: Option<u32>,
    pub session_id: Option<AgentSessionId>,
}

impl MessageEncoding for Ping {
    const STATIC_SIZE: Option<usize> = Some(8 + m_static::<Option<u32>>() + m_static::<Option<AgentSessionId>>());

    fn write_to<T: Write>(&self, out: &mut T) -> std::io::Result<usize> {
        let mut sum = 0;
        sum += self.now.write_to(out)?;
        sum += self.current_ping.write_to(out)?;
        sum += self.session_id.write_to(out)?;
        Ok(sum)
    }

    fn read_from<T: Read>(read: &mut T) -> std::io::Result<Self> {
        Ok(Ping {
            now: MessageEncoding::read_from(read)?,
            current_ping: MessageEncoding::read_from(read)?,
            session_id: MessageEncoding::read_from(read)?,
        })
    }
}


#[derive(Debug, Eq, PartialEq, Clone, Serialize)]
pub struct AgentRegister {
    pub proto_version: u64,
    pub account_id: u64,
    pub agent_id: u64,
    pub agent_version: u64,
    pub timestamp: u64,
    pub client_addr: SocketAddr,
    pub tunnel_addr: SocketAddr,
    pub signature: [u8; 32],
}

impl AgentRegister {
    pub fn update_signature(&mut self, temp_buffer: &mut Vec<u8>, hmac: &HmacSha256) {
        self.write_plain(temp_buffer);
        self.signature = hmac.sign(temp_buffer);
    }

    pub fn verify_signature(&self, temp_buffer: &mut Vec<u8>, hmac: &HmacSha256) -> bool {
        self.write_plain(temp_buffer);
        hmac.verify(temp_buffer, &self.signature).is_ok()
    }

    fn write_plain(&self, temp_buffer: &mut Vec<u8>) {
        temp_buffer.clear();
        self.write_to(temp_buffer).unwrap();
        assert!(self.signature.len() <= temp_buffer.len());

        let adjusted_len = temp_buffer.len() - self.signature.len();
        temp_buffer.truncate(adjusted_len);
    }
}

const ENCODING_INCLUDES_VERSION_BIT: u64 = 1u64 << 63;

impl MessageEncoding for AgentRegister {
    const MAX_SIZE: Option<usize> = m_opt_sum(&[
        u64::MAX_SIZE,
        u64::MAX_SIZE,
        u64::MAX_SIZE,
        u64::MAX_SIZE,
        u64::MAX_SIZE,
        SocketAddr::MAX_SIZE,
        SocketAddr::MAX_SIZE,
        Some(32),
    ]);

    fn write_to<T: Write>(&self, out: &mut T) -> std::io::Result<usize> {
        let mut sum = 0;

        if self.proto_version <= 1 {
            if (self.account_id & ENCODING_INCLUDES_VERSION_BIT) == ENCODING_INCLUDES_VERSION_BIT {
                return Err(std::io::Error::new(std::io::ErrorKind::InvalidInput, "account id too large for proto version 1"));
            }

            sum += self.account_id.write_to(out)?;
        } else {
            if (self.proto_version & ENCODING_INCLUDES_VERSION_BIT) == ENCODING_INCLUDES_VERSION_BIT {
                return Err(std::io::Error::new(std::io::ErrorKind::InvalidInput, "invalid proto version"));
            }

            sum += (self.proto_version | ENCODING_INCLUDES_VERSION_BIT).write_to(out)?;
            sum += self.account_id.write_to(out)?;
        }

        sum += self.agent_id.write_to(out)?;
        sum += self.agent_version.write_to(out)?;
        sum += self.timestamp.write_to(out)?;
        sum += self.client_addr.write_to(out)?;
        sum += self.tunnel_addr.write_to(out)?;
        out.write_all(&self.signature)?;
        sum += self.signature.len();
        Ok(sum)
    }

    fn read_from<T: Read>(read: &mut T) -> std::io::Result<Self> {
        let first_word = u64::read_from(read)?;

        let mut proto_version = 1;
        let account_id: u64;

        if (first_word & ENCODING_INCLUDES_VERSION_BIT) == ENCODING_INCLUDES_VERSION_BIT {
            proto_version = first_word & ENCODING_INCLUDES_VERSION_BIT.not();
            account_id = u64::read_from(read)?;
        } else {
            account_id = first_word;
        }

        let mut res = AgentRegister {
            proto_version,
            account_id,
            agent_id: u64::read_from(read)?,
            agent_version: u64::read_from(read)?,
            timestamp: u64::read_from(read)?,
            client_addr: SocketAddr::read_from(read)?,
            tunnel_addr: SocketAddr::read_from(read)?,
            signature: [0u8; 32],
        };

        read.read_exact(&mut res.signature[..])?;
        Ok(res)
    }
}

pub struct AgentRegisterV1 {
    pub account_id: u64,
    pub agent_id: u64,
    pub agent_version: u64,
    pub timestamp: u64,
    pub client_addr: SocketAddr,
    pub tunnel_addr: SocketAddr,
    pub signature: [u8; 32],
}

impl MessageEncoding for AgentRegisterV1 {
    fn write_to<T: Write>(&self, out: &mut T) -> std::io::Result<usize> {
        out.write_u64::<BigEndian>(self.account_id)?;
        out.write_u64::<BigEndian>(self.agent_id)?;
        out.write_u64::<BigEndian>(self.agent_version)?;
        out.write_u64::<BigEndian>(self.timestamp)?;
        let mut len = 8 + 8 + 8 + 8;
        len += self.client_addr.write_to(out)?;
        len += self.tunnel_addr.write_to(out)?;
        if out.write(&self.signature)? != 32 {
            return Err(std::io::Error::new(std::io::ErrorKind::WriteZero, "failed to write full signature"));
        }
        len += 32;
        Ok(len)
    }

    fn read_from<T: Read>(read: &mut T) -> std::io::Result<Self> {
        let mut res = Self {
            account_id: read.read_u64::<BigEndian>()?,
            agent_id: read.read_u64::<BigEndian>()?,
            agent_version: read.read_u64::<BigEndian>()?,
            timestamp: read.read_u64::<BigEndian>()?,
            client_addr: SocketAddr::read_from(read)?,
            tunnel_addr: SocketAddr::read_from(read)?,
            signature: [0u8; 32],
        };

        if read.read(&mut res.signature[..])? != 32 {
            return Err(std::io::Error::new(std::io::ErrorKind::UnexpectedEof, "missing signature"));
        }

        Ok(res)
    }
}

impl AgentRegisterV1 {
    pub fn upgrade(self) -> AgentRegister {
        AgentRegister {
            proto_version: 1,
            account_id: self.account_id,
            agent_id: self.agent_id,
            agent_version: self.agent_version,
            timestamp: self.timestamp,
            client_addr: self.client_addr,
            tunnel_addr: self.tunnel_addr,
            signature: self.signature,
        }
    }
}

#[derive(Debug, Eq, PartialEq, Clone, Serialize)]
pub enum ControlResponse {
    Pong(Pong),
    InvalidSignature,
    Unauthorized,
    RequestQueued,
    TryAgainLater,
    AgentRegistered(AgentRegistered),
    AgentPortMapping(AgentPortMapping),
    UdpChannelDetails(UdpChannelDetails),
}

impl MessageEncoding for ControlResponse {
    fn write_to<T: Write>(&self, out: &mut T) -> std::io::Result<usize> {
        let mut sum = 0;

        match self {
            ControlResponse::Pong(data) => {
                sum += 1u32.write_to(out)?;
                sum += data.write_to(out)?;
            }
            ControlResponse::InvalidSignature => {
                sum += 2u32.write_to(out)?;
            }
            ControlResponse::Unauthorized => {
                sum += 3u32.write_to(out)?;
            }
            ControlResponse::RequestQueued => {
                sum += 4u32.write_to(out)?;
            }
            ControlResponse::TryAgainLater => {
                sum += 5u32.write_to(out)?;
            }
            ControlResponse::AgentRegistered(data) => {
                sum += 6u32.write_to(out)?;
                sum += data.write_to(out)?;
            }
            ControlResponse::AgentPortMapping(data) => {
                sum += 7u32.write_to(out)?;
                sum += data.write_to(out)?;
            }
            ControlResponse::UdpChannelDetails(data) => {
                sum += 8u32.write_to(out)?;
                sum += data.write_to(out)?;
            }
        }

        Ok(sum)
    }

    fn read_from<T: Read>(read: &mut T) -> std::io::Result<Self> {
        match read.read_u32::<BigEndian>()? {
            1 => Ok(ControlResponse::Pong(Pong::read_from(read)?)),
            2 => Ok(ControlResponse::InvalidSignature),
            3 => Ok(ControlResponse::Unauthorized),
            4 => Ok(ControlResponse::RequestQueued),
            5 => Ok(ControlResponse::TryAgainLater),
            6 => Ok(ControlResponse::AgentRegistered(AgentRegistered::read_from(read)?)),
            7 => Ok(ControlResponse::AgentPortMapping(AgentPortMapping::read_from(read)?)),
            8 => Ok(ControlResponse::UdpChannelDetails(UdpChannelDetails::read_from(read)?)),
            _ => Err(std::io::Error::other("invalid ControlResponse id")),
        }
    }
}

#[derive(Debug, Eq, PartialEq, Clone, Serialize)]
pub struct AgentPortMapping {
    pub range: PortRange,
    pub found: Option<AgentPortMappingFound>,
}

impl MessageEncoding for AgentPortMapping {
    const MAX_SIZE: Option<usize> = Some(
        m_max::<PortRange>() +
        m_max::<Option<AgentPortMappingFound>>()
    );

    fn write_to<T: Write>(&self, out: &mut T) -> std::io::Result<usize> {
        let mut sum = 0;
        sum += self.range.write_to(out)?;
        sum += self.found.write_to(out)?;
        Ok(sum)
    }

    fn read_from<T: Read>(read: &mut T) -> std::io::Result<Self> {
        Ok(AgentPortMapping {
            range: PortRange::read_from(read)?,
            found: Option::<AgentPortMappingFound>::read_from(read)?,
        })
    }
}

#[derive(Debug, Eq, PartialEq, Clone, Serialize)]
pub enum AgentPortMappingFound {
    ToAgent(AgentSessionId),
}

impl MessageEncoding for AgentPortMappingFound {
    const MAX_SIZE: Option<usize> = Some(4 + m_max_list(&[
        m_max::<AgentSessionId>(),
    ]));
    
    fn write_to<T: Write>(&self, out: &mut T) -> std::io::Result<usize> {
        let mut sum = 0;

        match self {
            AgentPortMappingFound::ToAgent(id) => {
                sum += 1u32.write_to(out)?;
                sum += id.write_to(out)?;
            }
        }

        Ok(sum)
    }

    fn read_from<T: Read>(read: &mut T) -> std::io::Result<Self> {
        match read.read_u32::<BigEndian>()? {
            1 => Ok(AgentPortMappingFound::ToAgent(AgentSessionId::read_from(read)?)),
            _ => Err(std::io::Error::new(std::io::ErrorKind::Other, "unknown AgentPortMappingFound id")),
        }
    }
}

#[derive(Eq, PartialEq, Clone)]
pub struct UdpChannelDetails {
    pub tunnel_addr: SocketAddr,
    pub token: Arc<Vec<u8>>,
}

impl Serialize for UdpChannelDetails {
    fn serialize<S>(&self, serializer: S) -> Result<S::Ok, S::Error> where S: serde::Serializer {
        let mut s = serializer.serialize_struct("UdpChannelDetails", 2)?;
        s.serialize_field("tunnel_addr", &self.tunnel_addr)?;
        s.serialize_field("token", &*self.token)?;
        s.end()
    }
}

impl Debug for UdpChannelDetails {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("UdpChannelDetails")
            .field("tunnel_addr", &self.tunnel_addr)
            .field("token", &hex::encode(&self.token[..]))
            .finish()
    }
}

impl MessageEncoding for UdpChannelDetails {
    fn write_to<T: Write>(&self, out: &mut T) -> std::io::Result<usize> {
        let mut sum = 0;
        sum += self.tunnel_addr.write_to(out)?;
        sum += self.token.write_to(out)?;
        Ok(sum)
    }

    fn read_from<T: Read>(read: &mut T) -> std::io::Result<Self> {
        Ok(UdpChannelDetails {
            tunnel_addr: SocketAddr::read_from(read)?,
            token: Arc::new(Vec::read_from(read)?),
        })
    }
}

#[derive(Debug, Eq, PartialEq, Clone, Serialize)]
pub struct Pong {
    pub request_now: u64,
    pub server_now: u64,
    pub server_id: u64,
    pub data_center_id: u32,
    pub client_addr: SocketAddr,
    pub tunnel_addr: SocketAddr,
    pub session_expire_at: Option<u64>,
}

impl MessageEncoding for Pong {
    const MAX_SIZE: Option<usize> = Some(
        m_static::<u64>() * 3 +
        m_static::<u32>() +
        m_max::<SocketAddr>() * 2 +
        m_static::<Option<u64>>()
    );

    fn write_to<T: Write>(&self, out: &mut T) -> std::io::Result<usize> {
        let mut sum = 0;
        sum += self.request_now.write_to(out)?;
        sum += self.server_now.write_to(out)?;
        sum += self.server_id.write_to(out)?;
        sum += self.data_center_id.write_to(out)?;
        sum += self.client_addr.write_to(out)?;
        sum += self.tunnel_addr.write_to(out)?;
        sum += self.session_expire_at.write_to(out)?;
        Ok(sum)
    }

    fn read_from<T: Read>(read: &mut T) -> std::io::Result<Self> {
        Ok(Pong {
            request_now: read.read_u64::<BigEndian>()?,
            server_now: read.read_u64::<BigEndian>()?,
            server_id: read.read_u64::<BigEndian>()?,
            data_center_id: read.read_u32::<BigEndian>()?,
            client_addr: SocketAddr::read_from(read)?,
            tunnel_addr: SocketAddr::read_from(read)?,
            session_expire_at: Option::read_from(read)?,
        })
    }
}

#[derive(Debug, Eq, PartialEq, Clone, Serialize)]
pub struct AgentRegistered {
    pub id: AgentSessionId,
    pub expires_at: u64,
}

impl MessageEncoding for AgentRegistered {
    const STATIC_SIZE: Option<usize> = Some(
        m_static::<AgentSessionId>() +
        m_static::<u64>()
    );

    fn write_to<T: Write>(&self, out: &mut T) -> std::io::Result<usize> {
        let mut sum = 0;
        sum += self.id.write_to(out)?;
        sum += self.expires_at.write_to(out)?;
        Ok(sum)
    }

    fn read_from<T: Read>(read: &mut T) -> std::io::Result<Self> {
        Ok(AgentRegistered {
            id: AgentSessionId::read_from(read)?,
            expires_at: read.read_u64::<BigEndian>()?,
        })
    }
}

#[cfg(test)]
mod test {
    use std::fmt::Debug;
    use std::net::{IpAddr, Ipv4Addr};

    use rand::{Rng, RngCore, thread_rng};

    use crate::PortProto;
    use crate::rpc::ControlRpcMessage;

    use super::*;

    #[test]
    fn agent_register_sign_test() {
        let mut reg = AgentRegister {
            proto_version: 0,
            account_id: 1,
            agent_id: 2,
            agent_version: 3,
            timestamp: 1000,
            client_addr: "10.20.30.40:5678".parse().unwrap(),
            tunnel_addr: "9.20.3.40:9912".parse().unwrap(),
            signature: [0u8; 32],
        };

        let hmac = HmacSha256::create("this is a super secret secret".as_bytes());

        let mut buffer = Vec::new();
        reg.update_signature(&mut buffer, &hmac);
        assert!(reg.verify_signature(&mut buffer, &hmac));

        reg.proto_version = 1;
        reg.update_signature(&mut buffer, &hmac);
        assert!(reg.verify_signature(&mut buffer, &hmac));
    }

    #[test]
    fn agent_register_old_proto_decode() {
        let reg = AgentRegisterV1 {
            account_id: 1,
            agent_id: 2,
            agent_version: 3,
            timestamp: 1000,
            client_addr: "10.20.30.40:5678".parse().unwrap(),
            tunnel_addr: "9.20.3.40:9912".parse().unwrap(),
            signature: [0u8; 32],
        };

        let mut out = Vec::new();
        ControlRequestId::AgentRegisterV1.write_to(&mut out).unwrap();
        reg.write_to(&mut out).unwrap();

        let mut reader = &out[..];
        let read = ControlRequest::read_from(&mut reader).unwrap();
        assert_eq!(read, ControlRequest::AgentRegister(AgentRegister {
            proto_version: 1,
            account_id: 1,
            agent_id: 2,
            agent_version: 3,
            timestamp: 1000,
            client_addr: "10.20.30.40:5678".parse().unwrap(),
            tunnel_addr: "9.20.3.40:9912".parse().unwrap(),
            signature: [0u8; 32],
        }))
    }

    #[test]
    fn fuzzy_test_control_request() {
        let mut rng = thread_rng();
        let mut buffer = vec![0u8; 2048];

        for _ in 0..100000 {
            let msg = rng_control_request(&mut rng);
            test_encoding(msg, &mut buffer);
        }

        for _ in 0..1000 {
            test_encoding(ControlRpcMessage {
                request_id: rng.next_u64(),
                content: rng_control_request(&mut rng),
            }, &mut buffer);
        }
    }

    #[test]
    fn fuzzy_test_control_response() {
        let mut rng = thread_rng();
        let mut buffer = vec![0u8; 2048];

        for _ in 0..100000 {
            let msg = rng_control_response(&mut rng);
            test_encoding(msg, &mut buffer);
        }

        for _ in 0..1000 {
            test_encoding(ControlRpcMessage {
                request_id: rng.next_u64(),
                content: rng_control_response(&mut rng),
            }, &mut buffer);
        }
    }

    fn test_encoding<T: MessageEncoding + PartialEq + Debug>(msg: T, buffer: &mut [u8]) {
        assert_eq!(0, T::_ASSERT);

        let mut writer = &mut buffer[..];
        msg.write_to(&mut writer).unwrap();

        let remaining_len = writer.len();
        let written = buffer.len() - remaining_len;

        if let Some(size) =  T::STATIC_SIZE {
            assert_eq!(written, size);
        }

        if let Some(size) = T::MAX_SIZE {
            assert!(written <= size);
        }

        let mut reader = &buffer[0..written];
        let recovered = T::read_from(&mut reader).unwrap();

        assert_eq!(msg, recovered);
    }

    pub fn rng_control_request<R: RngCore>(rng: &mut R) -> ControlRequest {
        match rng.next_u32() % 5 {
            0 => ControlRequest::Ping(Ping {
                now: rng.next_u64(),
                current_ping: if rng.next_u32() % 2 == 0 {
                    Some(rng.next_u32())
                } else {
                    None
                },
                session_id: if rng.next_u32() % 2 == 0 {
                    Some(AgentSessionId {
                        session_id: rng.next_u64(),
                        account_id: rng.next_u64() % (i64::MAX as u64),
                        agent_id: rng.next_u64(),
                    })
                } else {
                    None
                },
            }),
            1 => ControlRequest::AgentRegister(AgentRegister {
                proto_version: 1 + rng.next_u64() % 2,
                account_id: rng.next_u64() % (i64::MAX as u64),
                agent_id: rng.next_u64(),
                agent_version: rng.next_u64(),
                timestamp: rng.next_u64(),
                client_addr: rng_socket_address(rng),
                tunnel_addr: rng_socket_address(rng),
                signature: {
                    let mut bytes = [0u8; 32];
                    rng.fill(&mut bytes);
                    bytes
                },
            }),
            2 => ControlRequest::AgentKeepAlive(AgentSessionId {
                session_id: rng.next_u64(),
                account_id: rng.next_u64() % (i64::MAX as u64),
                agent_id: rng.next_u64(),
            }),
            3 => ControlRequest::SetupUdpChannel(AgentSessionId {
                session_id: rng.next_u64(),
                account_id: rng.next_u64() % (i64::MAX as u64),
                agent_id: rng.next_u64(),
            }),
            4 => ControlRequest::AgentCheckPortMapping(AgentCheckPortMapping {
                agent_session_id: AgentSessionId {
                    session_id: rng.next_u64(),
                    account_id: rng.next_u64() % (i64::MAX as u64),
                    agent_id: rng.next_u64(),
                },
                port_range: PortRange {
                    ip: match rng.next_u32() % 2 {
                        0 => IpAddr::V4(Ipv4Addr::from(rng.next_u32())),
                        1 => IpAddr::V6({
                            let mut bytes = [0u8; 16];
                            rng.fill(&mut bytes);
                            bytes.into()
                        }),
                        _ => unreachable!(),
                    },
                    port_start: rng.next_u32() as u16,
                    port_end: rng.next_u32() as u16,
                    port_proto: match rng.next_u32() % 3 {
                        0 => PortProto::Tcp,
                        1 => PortProto::Udp,
                        2 => PortProto::Both,
                        _ => unreachable!(),
                    },
                },
            }),
            _ => unreachable!(),
        }
    }

    pub fn rng_control_response<R: RngCore>(rng: &mut R) -> ControlResponse {
        match rng.next_u32() % 8 {
            0 => ControlResponse::Pong(Pong {
                request_now: rng.next_u64(),
                server_now: rng.next_u64(),
                server_id: rng.next_u64(),
                data_center_id: rng.next_u32(),
                client_addr: rng_socket_address(rng),
                tunnel_addr: rng_socket_address(rng),
                session_expire_at: if rng.next_u32() % 2 == 1 {
                    Some(rng.next_u64())
                } else {
                    None
                },
            }),
            1 => ControlResponse::InvalidSignature,
            2 => ControlResponse::Unauthorized,
            3 => ControlResponse::RequestQueued,
            4 => ControlResponse::TryAgainLater,
            5 => ControlResponse::AgentRegistered(AgentRegistered {
                id: AgentSessionId {
                    session_id: rng.next_u64(),
                    account_id: rng.next_u64() % (i64::MAX as u64),
                    agent_id: rng.next_u64(),
                },
                expires_at: rng.next_u64(),
            }),
            6 => ControlResponse::AgentPortMapping(AgentPortMapping {
                range: PortRange {
                    ip: match rng.next_u32() % 2 {
                        0 => IpAddr::V4(Ipv4Addr::from(rng.next_u32())),
                        1 => IpAddr::V6({
                            let mut bytes = [0u8; 16];
                            rng.fill(&mut bytes);
                            bytes.into()
                        }),
                        _ => unreachable!(),
                    },
                    port_start: rng.next_u32() as u16,
                    port_end: rng.next_u32() as u16,
                    port_proto: match rng.next_u32() % 3 {
                        0 => PortProto::Tcp,
                        1 => PortProto::Udp,
                        2 => PortProto::Both,
                        _ => unreachable!(),
                    },
                },
                found: match rng.next_u32() % 2 {
                    0 => None,
                    1 => Some(AgentPortMappingFound::ToAgent(AgentSessionId {
                        session_id: rng.next_u64(),
                        account_id: rng.next_u64() % (i64::MAX as u64),
                        agent_id: rng.next_u64(),
                    })),
                    _ => unreachable!()
                },
            }),
            7 => ControlResponse::UdpChannelDetails(UdpChannelDetails {
                tunnel_addr: rng_socket_address(rng),
                token: {
                    let len = ((rng.next_u64() % 30) + 32) as usize;
                    let mut buffer = vec![0u8; len];
                    rng.fill_bytes(&mut buffer);
                    Arc::new(buffer)
                },
            }),
            _ => unreachable!()
        }
    }

    fn rng_socket_address<R: RngCore>(rng: &mut R) -> SocketAddr {
        SocketAddr::new(
            match rng.next_u32() % 2 {
                0 => IpAddr::V4(Ipv4Addr::from(rng.next_u32())),
                1 => IpAddr::V6({
                    let mut bytes = [0u8; 16];
                    rng.fill(&mut bytes);
                    bytes.into()
                }),
                _ => unreachable!(),
            },
            rng.next_u32() as u16,
        )
    }

    #[test]
    fn agent_register_v1_ip4_same_encoding_test() {
        let mut msg = AgentRegister {
            account_id: 100,
            agent_id: 32,
            agent_version: 676,
            timestamp: 103201401,
            client_addr: "127.0.0.1:4123".parse().unwrap(),
            tunnel_addr: "99.12.34.51:5312".parse().unwrap(),
            signature: [0u8; 32],
            proto_version: 1,
        };

        let sig = HmacSha256::create("test-secret-hehehe".as_bytes());
        let mut buffer = Vec::new();
        msg.update_signature(&mut buffer, &sig);
        assert!(msg.verify_signature(&mut buffer, &sig));

        buffer.clear();
        msg.write_to(&mut buffer).unwrap();

        let hex_buffer = hex::encode(&buffer);
        assert_eq!(hex_buffer, "0000000000000064000000000000002000000000000002a4000000000626ba79047f000001101b04630c223314c0767a59319b8edfcc1e6f3d3ea2d19ac74a74e5f5333c9b335adc72cda821de5f");
    }

    #[test]
    fn agent_register_v1_ip6_same_encoding_test() {
        let mut msg = AgentRegister {
            account_id: 100,
            agent_id: 32,
            agent_version: 676,
            timestamp: 103201401,
            client_addr: "[::88]:4123".parse().unwrap(),
            tunnel_addr: "[::99]:5312".parse().unwrap(),
            signature: [0u8; 32],
            proto_version: 1,
        };

        let sig = HmacSha256::create("test-secret-hehehe".as_bytes());
        let mut buffer = Vec::new();
        msg.update_signature(&mut buffer, &sig);
        assert!(msg.verify_signature(&mut buffer, &sig));

        buffer.clear();
        msg.write_to(&mut buffer).unwrap();

        let hex_buffer = hex::encode(&buffer);
        assert_eq!(hex_buffer, "0000000000000064000000000000002000000000000002a4000000000626ba790600000000000000000000000000000088101b060000000000000000000000000000009914c0724f203e7ac2f090800dbeb68afbf184f367f9ca14d8a0082e245070c3835c4b");
    }

    #[test]
    fn legacy_mc_java_ping_decode_test() {
        let data = hex::decode("000000000000000100000001000000000000000000").unwrap();
        let mut reader = &data[..];

        let msg = ControlRpcMessage::<ControlRequest>::read_from(&mut reader).unwrap();
        assert_eq!(msg, ControlRpcMessage {
            request_id: 1,
            content: ControlRequest::Ping(Ping {
                now: 0,
                current_ping: None,
                session_id: None,
            }),
        });
        assert_eq!(reader.len(), 0);
        println!("Got msg: {msg:?}");
    }
}
