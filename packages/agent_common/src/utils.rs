use std::net::Ipv6Addr;
use byteorder::{BigEndian, ReadBytesExt};

pub fn shuffle(mut v: u32) -> u32 {
    v = ((v >> 16) ^ v).overflowing_mul(0x45d9f3u32).0;
    v = ((v >> 16) ^ v).overflowing_mul(0x45d9f3u32).0;
    v = (v >> 16) ^ v;
    v
}

pub fn shuffle_slice(mut bytes: &[u8]) -> u32 {
    let mut v = 0;

    while bytes.len() > 4 {
        v ^= shuffle(bytes.read_u32::<BigEndian>().unwrap());
    }
    for byte in bytes {
        v ^= shuffle(*byte as u32);
    }

    v
}