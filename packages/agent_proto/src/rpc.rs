use std::io::{Read, Write};
use byteorder::{BigEndian, ReadBytesExt};
use crate::MessageEncoding;

#[derive(Debug, Eq, PartialEq, Clone)]
pub struct ControlRpcMessage<T: MessageEncoding> {
    pub request_id: u64,
    pub content: T,
}

impl<T: MessageEncoding> MessageEncoding for ControlRpcMessage<T> {
    const STATIC_SIZE: Option<usize> = match T::STATIC_SIZE {
        Some(v) => Some(8 + v),
        None => None,
    };

    const MAX_SIZE: Option<usize> = match T::MAX_SIZE {
        Some(v) => Some(8 + v),
        None => None,
    };

    fn write_to<I: Write>(&self, out: &mut I) -> std::io::Result<usize> {
        let mut sum = 0;
        sum += self.request_id.write_to(out)?;
        sum += self.content.write_to(out)?;
        Ok(sum)
    }

    fn read_from<I: Read>(read: &mut I) -> std::io::Result<Self> {
        Ok(ControlRpcMessage {
            request_id: read.read_u64::<BigEndian>()?,
            content: T::read_from(read)?,
        })
    }
}
