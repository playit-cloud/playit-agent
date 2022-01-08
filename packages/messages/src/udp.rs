use byteorder::{BigEndian, ByteOrder};
use std::net::SocketAddrV4;

pub const REDIRECT_FLOW_FOOTER_ID: u64 = 0x5cb867cf788173b2;
pub const UDP_CHANNEL_ESTABLISH_ID: u64 = 0xd01fe6830ddce781;

#[derive(Copy, Clone, PartialEq, PartialOrd, Ord, Eq, Debug)]
pub struct RedirectFlowFooter {
  pub src: SocketAddrV4,
  pub dst: SocketAddrV4,
}

impl RedirectFlowFooter {
  pub fn flip(self) -> RedirectFlowFooter {
    RedirectFlowFooter {
      src: self.dst,
      dst: self.src,
    }
  }

  pub fn write_to(&self, slice: &mut [u8]) -> bool {
    if slice.len() < Self::len() {
      return false;
    }

    BigEndian::write_u32(&mut slice[0..4], (*self.src.ip()).into());
    BigEndian::write_u32(&mut slice[4..8], (*self.dst.ip()).into());
    BigEndian::write_u16(&mut slice[8..10], self.src.port());
    BigEndian::write_u16(&mut slice[10..12], self.dst.port());
    BigEndian::write_u64(&mut slice[12..20], REDIRECT_FLOW_FOOTER_ID);

    true
  }

  pub fn from_tail(slice: &[u8]) -> Option<RedirectFlowFooter> {
    let mut len = slice.len();
    if len < Self::len() {
      return None;
    }

    let id = BigEndian::read_u64(&slice[len - 8..]);
    if id != REDIRECT_FLOW_FOOTER_ID {
      return None;
    }

    len -= 8;

    let src_ip = BigEndian::read_u32(&slice[len - 12..len - 8]);
    let dst_ip = BigEndian::read_u32(&slice[len - 8..len - 4]);
    let src_port = BigEndian::read_u16(&slice[len - 4..len - 2]);
    let dst_port = BigEndian::read_u16(&slice[len - 2..len]);

    Some(RedirectFlowFooter {
      src: SocketAddrV4::new(src_ip.into(), src_port),
      dst: SocketAddrV4::new(dst_ip.into(), dst_port),
    })
  }

  pub fn len() -> usize {
    20
  }
}

#[cfg(test)]
mod test {
  use crate::udp::RedirectFlowFooter;

  #[test]
  fn test_serialize() {
    let mut buf = [0u8; 100];
    let flow = RedirectFlowFooter {
      src: "123.234.13.43:8891".parse().unwrap(),
      dst: "123.99.13.43:773".parse().unwrap(),
    };
    flow.write_to(&mut buf[100 - 20..]);

    let parsed = RedirectFlowFooter::from_tail(&buf).unwrap();
    assert_eq!(flow, parsed);
  }

  #[test]
  fn test_parse() {
    let data = hex::decode("746573740a93b9ddc0c0a811b304d2ada35cb867cf788173b2").unwrap();
    let parsed = RedirectFlowFooter::from_tail(&data).unwrap();
    println!("Data: {:?}", parsed);
  }
}
