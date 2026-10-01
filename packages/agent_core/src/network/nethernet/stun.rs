pub struct Stun;

impl Stun {
    const HEADER_LEN: usize = 20;
    const MAGIC_COOKIE: [u8; 4] = [0x21, 0x12, 0xA4, 0x42];
    const BINDING_REQUEST: u16 = 0x0001;
    const ATTR_USERNAME: u16 = 0x0006;

    /// The STUN packets ICE sends are around 100 bytes.
    pub const MAX_MESSAGE_LEN: usize = 512;

    /// RFC 8839 allows 256 ice-chars.
    pub const MAX_UFRAG_LEN: usize = 256;

    /// STUN header, 20 bytes:
    ///   0..2   message type, top two bits always zero
    ///   2..4   body length, not counting the header
    ///   4..8   magic cookie 0x2112A442
    ///   8..20  transaction id
    pub fn looks_like_stun(data: &[u8]) -> bool {
        data.len() >= Self::HEADER_LEN && data[0] & 0xC0 == 0 && data[4..8] == Self::MAGIC_COOKIE
    }

    /// Message type 0x0101 is Binding success.
    pub fn is_binding_success(data: &[u8]) -> bool {
        Self::looks_like_stun(data)
            && data[..2] == [1, 1]
            && data.len() == Self::HEADER_LEN + u16::from_be_bytes([data[2], data[3]]) as usize
    }

    /// One attribute, 4-byte aligned:
    ///   0..2  type
    ///   2..4  value length, before padding
    ///   4..   value, then zero padding up to a multiple of 4
    ///
    /// Returns the type, the value, and the bytes after the padding.
    fn parse_attribute(data: &[u8]) -> Option<(u16, &[u8], &[u8])> {
        let (header, after_header) = data.split_at_checked(4)?;
        let attr_type = u16::from_be_bytes([header[0], header[1]]);
        let attr_len = u16::from_be_bytes([header[2], header[3]]) as usize;
        let (value, _) = after_header.split_at_checked(attr_len)?;
        let (_, rest) = after_header.split_at_checked(attr_len.div_ceil(4) * 4)?;
        Some((attr_type, value, rest))
    }

    /// `<bedrock ufrag>:<client ufrag>`. Example `rJb7:SWk/`
    fn bedrock_ufrag_from_username(username: &[u8]) -> Option<&[u8]> {
        let colon = username.iter().position(|&c| c == b':')?;
        if colon == 0 || colon + 1 >= username.len() {
            return None;
        }
        Some(&username[..colon])
    }

    /// Get the Bedrock server's ufrag from a Bedrock client's STUN packet.
    pub fn bedrock_ufrag(data: &[u8]) -> Option<&[u8]> {
        if !Self::looks_like_stun(data) || data.len() > Self::MAX_MESSAGE_LEN {
            return None;
        }
        if u16::from_be_bytes([data[0], data[1]]) != Self::BINDING_REQUEST {
            return None;
        }

        let body_len = u16::from_be_bytes([data[2], data[3]]) as usize;
        if !body_len.is_multiple_of(4) || data.len() != Self::HEADER_LEN + body_len {
            return None;
        }

        let mut attributes = &data[Self::HEADER_LEN..];
        while !attributes.is_empty() {
            let (attr_type, value, rest) = Self::parse_attribute(attributes)?;
            if attr_type == Self::ATTR_USERNAME {
                let ufrag = Self::bedrock_ufrag_from_username(value)?;
                if ufrag.len() > Self::MAX_UFRAG_LEN {
                    return None;
                }
                return Some(ufrag);
            }
            attributes = rest;
        }
        None
    }
}

#[cfg(test)]
mod test {
    use super::*;

    /* a Bedrock client's first Binding request, captured; USERNAME is rJb7:SWk/ */
    const CLIENT_CHECK: &str = "000100502112a4425949507037627963376f434200060009724a62373a53576b2f000000c057000400020000802a0008a7b6e8826927321100250000002400046e7e1eff0008001472cc63fe85b3a06cf6913cc142d35fd928131d9c802800043562cf76";

    #[test]
    fn reads_bedrock_ufrag_from_a_binding_request() {
        let check = hex::decode(CLIENT_CHECK).unwrap();
        assert!(Stun::looks_like_stun(&check));
        assert_eq!(Stun::bedrock_ufrag(&check), Some(&b"rJb7"[..]));

        /* a success response has the same shape but is not a request */
        let mut response = check.clone();
        response[0] = 0x01;
        assert!(Stun::looks_like_stun(&response));
        assert_eq!(Stun::bedrock_ufrag(&response), None);

        /* a length that does not match the header is refused */
        let mut short = check.clone();
        short.pop();
        assert_eq!(Stun::bedrock_ufrag(&short), None);

        assert!(!Stun::looks_like_stun(&[0x01; 33]));
    }
}
