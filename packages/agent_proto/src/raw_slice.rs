use std::io::{Read, Write};

use message_encoding::MessageEncoding;

pub struct RawSlice<'a>(pub &'a [u8]);

impl MessageEncoding for RawSlice<'_> {
    fn write_to<T: Write>(&self, out: &mut T) -> std::io::Result<usize> {
        if out.write(self.0)? != self.0.len() {
            return Err(std::io::Error::new(
                std::io::ErrorKind::WriteZero,
                "not enough space to write raw slice",
            ));
        }
        Ok(self.0.len())
    }

    fn read_from<T: Read>(_: &mut T) -> std::io::Result<Self> {
        Err(std::io::Error::new(
            std::io::ErrorKind::Unsupported,
            "cannot read for RawSlice",
        ))
    }
}
