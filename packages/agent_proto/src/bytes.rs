use message_encoding::MessageEncoding;
use std::io::{self, Read, Write};

// Control tokens must fit within one UDP datagram, including their enclosing message.
const MAX_TOKEN_LEN: usize = 65_507;

pub(crate) fn read_token(read: &mut impl Read) -> io::Result<Vec<u8>> {
    let len = u64::read_from(read)?;
    if len > MAX_TOKEN_LEN as u64 {
        return Err(io::Error::new(
            io::ErrorKind::InvalidData,
            "control token exceeds datagram limit",
        ));
    }
    let mut token = vec![0; len as usize];
    read.read_exact(&mut token)?;
    Ok(token)
}

pub(crate) fn write_token(token: &[u8], out: &mut impl Write) -> io::Result<usize> {
    if token.len() > MAX_TOKEN_LEN {
        return Err(io::Error::new(
            io::ErrorKind::InvalidInput,
            "control token exceeds datagram limit",
        ));
    }
    (token.len() as u64).write_to(out)?;
    out.write_all(token)?;
    Ok(8 + token.len())
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn rejects_unbounded_length_before_allocation() {
        assert_eq!(
            read_token(&mut &u64::MAX.to_be_bytes()[..])
                .unwrap_err()
                .kind(),
            io::ErrorKind::InvalidData
        );
    }
    #[test]
    fn accepts_partial_reads_and_writes() {
        struct Slow<T>(T);
        impl<T: Read> Read for Slow<T> {
            fn read(&mut self, out: &mut [u8]) -> io::Result<usize> {
                let len = out.len().min(1);
                self.0.read(&mut out[..len])
            }
        }
        impl<T: Write> Write for Slow<T> {
            fn write(&mut self, data: &[u8]) -> io::Result<usize> {
                self.0.write(&data[..data.len().min(1)])
            }
            fn flush(&mut self) -> io::Result<()> {
                self.0.flush()
            }
        }
        let mut out = Slow(Vec::new());
        write_token(b"token", &mut out).unwrap();
        assert_eq!(read_token(&mut Slow(&out.0[..])).unwrap(), b"token");
    }
}
