use std::net::{Ipv4Addr, Ipv6Addr};

pub trait ReadIpBytesExt {
    fn read_ip4(&mut self) -> Result<Ipv4Addr, std::io::Error>;

    fn read_ip6(&mut self) -> Result<Ipv6Addr, std::io::Error>;
}

impl<T: std::io::Read> ReadIpBytesExt for T {
    fn read_ip4(&mut self) -> Result<Ipv4Addr, std::io::Error> {
        let mut bytes = [0u8; 4];
        self.read_exact(&mut bytes)?;
        Ok(Ipv4Addr::from(bytes))
    }

    fn read_ip6(&mut self) -> Result<Ipv6Addr, std::io::Error> {
        let mut bytes = [0u8; 16];
        self.read_exact(&mut bytes)?;
        Ok(Ipv6Addr::from(bytes))
    }
}