use std::fmt::{Debug, Formatter};
use std::io::{Error, ErrorKind, Read, Write};
use std::net::SocketAddr;

use byteorder::{BigEndian, ReadBytesExt};
use message_encoding::MessageEncoding;

use crate::control_messages::ControlResponse;
use crate::rpc::ControlRpcMessage;

#[derive(Debug, Eq, PartialEq, Clone)]
pub enum ControlFeed {
    Response(ControlRpcMessage<ControlResponse>),
    NewClient(NewClient),
}

#[derive(Debug, Eq, PartialEq, Clone)]
pub struct NewClient {
    pub connect_addr: SocketAddr,
    pub peer_addr: SocketAddr,
    pub claim_instructions: ClaimInstructions,
    pub tunnel_server_id: u64,
    pub data_center_id: u32,
}

#[derive(Eq, PartialEq, Clone)]
pub struct ClaimInstructions {
    pub address: SocketAddr,
    pub token: Vec<u8>,
}

impl Debug for ClaimInstructions {
    fn fmt(&self, f: &mut Formatter<'_>) -> std::fmt::Result {
        write!(f, "ClaimInstructions {{ address: {}, token: {} }}", self.address, hex::encode(&self.token))
    }
}

impl MessageEncoding for ControlFeed {
    fn write_to<T: Write>(&self, out: &mut T) -> std::io::Result<usize> {
        let mut sum = 0;

        match self {
            ControlFeed::Response(res) => {
                sum += 1u32.write_to(out)?;
                sum += res.write_to(out)?;
            }
            ControlFeed::NewClient(client) => {
                sum += 2u32.write_to(out)?;
                sum += client.write_to(out)?;
            }
        }

        Ok(sum)
    }

    fn read_from<T: Read>(read: &mut T) -> std::io::Result<Self> {
        match read.read_u32::<BigEndian>()? {
            1 => Ok(ControlFeed::Response(ControlRpcMessage::read_from(read)?)),
            2 => Ok(ControlFeed::NewClient(NewClient::read_from(read)?)),
            _ => Err(Error::new(ErrorKind::Other, "invalid ControlFeed id")),
        }
    }
}

impl MessageEncoding for NewClient {
    fn write_to<T: Write>(&self, out: &mut T) -> std::io::Result<usize> {
        let mut sum = 0;
        sum += self.connect_addr.write_to(out)?;
        sum += self.peer_addr.write_to(out)?;
        sum += self.claim_instructions.write_to(out)?;
        sum += self.tunnel_server_id.write_to(out)?;
        sum += self.data_center_id.write_to(out)?;
        Ok(sum)
    }

    fn read_from<T: Read>(read: &mut T) -> std::io::Result<Self> {
        Ok(NewClient {
            connect_addr: SocketAddr::read_from(read)?,
            peer_addr: SocketAddr::read_from(read)?,
            claim_instructions: ClaimInstructions::read_from(read)?,
            tunnel_server_id: read.read_u64::<BigEndian>()?,
            data_center_id: read.read_u32::<BigEndian>()?,
        })
    }
}

impl MessageEncoding for ClaimInstructions {
    fn write_to<T: Write>(&self, out: &mut T) -> std::io::Result<usize> {
        let mut sum = 0;
        sum += self.address.write_to(out)?;
        sum += self.token.write_to(out)?;
        Ok(sum)
    }

    fn read_from<T: Read>(read: &mut T) -> std::io::Result<Self> {
        Ok(ClaimInstructions {
            address: SocketAddr::read_from(read)?,
            token: Vec::read_from(read)?,
        })
    }
}

#[cfg(test)]
mod test {
    use super::*;

    #[test]
    fn parse_control() {
        let data = "0000000204d1198d10046804d053c766cc4904d1198c029306000000000000004c2\
        c003cd1198d100468d053c766cc49cba8329c930664e9431200000000000000010000000000298c05779c\
        9306000000000e00000000000065b2000171012de0fdb1b6d5de58be82911b07bc00000000000065b20000000e";

        let bytes = hex::decode(data).unwrap();
        let mut reader = &bytes[..];
        let req = ControlFeed::read_from(&mut reader).unwrap();
        println!("{:?}", req);
    }
}