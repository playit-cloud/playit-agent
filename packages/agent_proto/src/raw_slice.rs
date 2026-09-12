use std::io::{Read, Write};

use message_encoding::MessageEncoding;

pub struct RawSlice<'a>(pub &'a [u8]);

impl MessageEncoding for RawSlice<'_> {
    fn write_to<T: Write>(&self, out: &mut T) -> std::io::Result<usize> {
        out.write_all(self.0)?;
        Ok(self.0.len())
    }

    fn read_from<T: Read>(_: &mut T) -> std::io::Result<Self> {
        Err(std::io::Error::new(
            std::io::ErrorKind::Unsupported,
            "cannot read for RawSlice",
        ))
    }
}
