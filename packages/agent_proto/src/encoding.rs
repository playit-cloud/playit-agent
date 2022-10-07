use std::io::{Error, ErrorKind, Read, Result, Write};
use std::net::{IpAddr, Ipv4Addr, Ipv6Addr, SocketAddr, SocketAddrV4, SocketAddrV6};

use byteorder::{BigEndian, ReadBytesExt, WriteBytesExt};

pub trait MessageEncoding: Sized {
    fn write_to<T: Write>(&self, out: &mut T) -> Result<()>;

    fn read_from<T: Read>(read: &mut T) -> Result<Self>;
}

impl MessageEncoding for u64 {
    fn write_to<T: Write>(&self, out: &mut T) -> Result<()> {
        out.write_u64::<BigEndian>(*self)
    }

    fn read_from<T: Read>(read: &mut T) -> Result<Self> {
        read.read_u64::<BigEndian>()
    }
}

impl<T: MessageEncoding> MessageEncoding for Option<T> {
    fn write_to<I: Write>(&self, out: &mut I) -> Result<()> {
        match self {
            Some(v) => {
                out.write_u8(1)?;
                v.write_to(out)
            }
            None => {
                out.write_u8(0)
            }
        }
    }

    fn read_from<I: Read>(read: &mut I) -> Result<Self> {
        match read.read_u8()? {
            0 => Ok(None),
            1 => Ok(Some(T::read_from(read)?)),
            _ => Err(Error::new(ErrorKind::Other, "invalid Option value")),
        }
    }
}

impl MessageEncoding for IpAddr {
    fn write_to<T: Write>(&self, out: &mut T) -> Result<()> {
        match self {
            IpAddr::V4(ip) => {
                out.write_u8(4)?;
                ip.write_to(out)
            }
            IpAddr::V6(ip) => {
                out.write_u8(6)?;
                ip.write_to(out)
            }
        }
    }

    fn read_from<T: Read>(read: &mut T) -> Result<Self> {
        match read.read_u8()? {
            4 => {
                Ok(IpAddr::V4(Ipv4Addr::read_from(read)?))
            }
            6 => {
                Ok(IpAddr::V6(Ipv6Addr::read_from(read)?))
            }
            v => Err(Error::new(ErrorKind::Other, format!("invalid ip type: {}", v))),
        }
    }
}

impl MessageEncoding for SocketAddr {
    fn write_to<T: Write>(&self, out: &mut T) -> Result<()> {
        match self {
            SocketAddr::V4(addr) => {
                out.write_u8(4)?;
                addr.ip().write_to(out)?;
                out.write_u16::<BigEndian>(addr.port())
            }
            SocketAddr::V6(addr) => {
                out.write_u8(6)?;
                addr.ip().write_to(out)?;
                out.write_u16::<BigEndian>(addr.port())
            }
        }
    }

    fn read_from<T: Read>(read: &mut T) -> Result<Self> {
        match read.read_u8()? {
            4 => Ok(SocketAddr::V4(SocketAddrV4::new(
                Ipv4Addr::read_from(read)?,
                read.read_u16::<BigEndian>()?,
            ))),
            6 => Ok(SocketAddr::V6(SocketAddrV6::new(
                Ipv6Addr::read_from(read)?,
                read.read_u16::<BigEndian>()?,
                0, 0
            ))),
            v => Err(Error::new(ErrorKind::Other, format!("invalid ip type: {}", v))),
        }
    }
}

impl MessageEncoding for Ipv4Addr {
    fn write_to<T: Write>(&self, out: &mut T) -> Result<()> {
        if out.write(&self.octets())? != 4 {
            return Err(Error::new(ErrorKind::WriteZero, "failed to write full ip"));
        }
        Ok(())
    }

    fn read_from<T: Read>(read: &mut T) -> Result<Self> {
        let mut bytes = [0u8; 4];
        if read.read(&mut bytes)? != 4 {
            return Err(Error::new(ErrorKind::UnexpectedEof, "missing ip4 data"));
        }
        Ok(Ipv4Addr::from(bytes))
    }
}

impl MessageEncoding for Ipv6Addr {
    fn write_to<T: Write>(&self, out: &mut T) -> Result<()> {
        if out.write(&self.octets())? != 16 {
            return Err(Error::new(ErrorKind::WriteZero, "failed to write full ip"));
        }
        Ok(())
    }

    fn read_from<T: Read>(read: &mut T) -> Result<Self> {
        let mut bytes = [0u8; 16];
        if read.read(&mut bytes)? != 16 {
            return Err(Error::new(ErrorKind::UnexpectedEof, "missing ip6 data"));
        }
        Ok(Ipv6Addr::from(bytes))
    }
}

impl MessageEncoding for Vec<u8> {
    fn write_to<T: Write>(&self, out: &mut T) -> Result<()> {
        out.write_u64::<BigEndian>(self.len() as _)?;
        if out.write(&self)? != self.len() {
            return Err(Error::new(ErrorKind::WriteZero, "failed to write entire array"));
        }
        Ok(())
    }

    fn read_from<T: Read>(read: &mut T) -> Result<Self> {
        let len = read.read_u64::<BigEndian>()? as usize;
        let mut data = vec![0u8; len];
        if read.read(&mut data)? != len {
            return Err(Error::new(ErrorKind::UnexpectedEof, "not enough data for array"));
        }
        Ok(data)
    }
}