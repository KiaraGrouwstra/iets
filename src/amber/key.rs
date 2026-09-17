//! The amber-store lookup key (core-rs `src/key.rs`, `architecture/keys.md`): 32 bytes, a
//! header byte (type in the high nibble, length-field size minus one in the low 3 bits), the
//! logical length big-endian without leading zeros, then the BLAKE3 digest of the object's
//! bytes truncated to fill the rest.

use std::fmt;

pub const SIZE: usize = 32;

#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash)]
#[repr(u8)]
pub enum Type {
    Blob = 0,
    FileNode = 1,
    DirLeaf = 2,
    DirNode = 3,
    XattrSet = 4,
}

impl Type {
    pub fn from_u8(v: u8) -> Option<Type> {
        match v {
            0 => Some(Type::Blob),
            1 => Some(Type::FileNode),
            2 => Some(Type::DirLeaf),
            3 => Some(Type::DirNode),
            4 => Some(Type::XattrSet),
            _ => None,
        }
    }
}

impl fmt::Display for Type {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(match self {
            Type::Blob => "Blob",
            Type::FileNode => "FileNode",
            Type::DirLeaf => "DirLeaf",
            Type::DirNode => "DirNode",
            Type::XattrSet => "XattrSet",
        })
    }
}

#[derive(Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub struct Key(pub [u8; SIZE]);

fn length_size_for(length: u64) -> usize {
    if length == 0 {
        return 1;
    }
    (u64::BITS - length.leading_zeros()).div_ceil(8) as usize
}

impl Key {
    /// BLAKE3 of `bytes`, then `new_from_hash`. `length` is the logical length (`Type` decides
    /// what that is), not necessarily `bytes.len()`.
    pub fn new(t: Type, length: u64, bytes: &[u8]) -> Key {
        Key::new_from_hash(t, length, *blake3::hash(bytes).as_bytes())
    }

    pub fn new_from_hash(t: Type, length: u64, full_hash: [u8; SIZE]) -> Key {
        let ls = length_size_for(length);
        let mut k = [0u8; SIZE];
        k[0] = (t as u8) << 4 | (ls as u8 - 1);
        let buf = length.to_be_bytes();
        k[1..1 + ls].copy_from_slice(&buf[8 - ls..]);
        k[1 + ls..].copy_from_slice(&full_hash[..SIZE - 1 - ls]);
        Key(k)
    }

    /// Accepts exactly 32 canonical bytes: reserved bit clear, a defined type, and a length
    /// field without leading zeros.
    pub fn parse(b: &[u8]) -> Result<Key, String> {
        if b.len() != SIZE {
            return Err(format!("key: data is not 32 bytes: got {}", b.len()));
        }
        let mut k = [0u8; SIZE];
        k.copy_from_slice(b);
        let k = Key(k);
        if k.0[0] & 0x08 != 0 {
            return Err("key: reserved header bit is set".into());
        }
        let raw = k.0[0] >> 4;
        if Type::from_u8(raw).is_none() {
            return Err(format!("key: reserved object type: {raw}"));
        }
        if k.0[1] == 0 && !(k.length_size() == 1 && k.length() == 0) {
            return Err("key: non-canonical length encoding".into());
        }
        Ok(k)
    }

    pub fn parse_hex(s: &str) -> Result<Key, String> {
        let bytes = crate::store::hash::base16_decode(s).map_err(|e| e.0)?;
        Key::parse(&bytes)
    }

    pub fn type_(&self) -> Type {
        Type::from_u8(self.0[0] >> 4).expect("canonical key")
    }

    pub fn length_size(&self) -> usize {
        (self.0[0] & 0x07) as usize + 1
    }

    pub fn length(&self) -> u64 {
        let ls = self.length_size();
        let mut buf = [0u8; 8];
        buf[8 - ls..].copy_from_slice(&self.0[1..1 + ls]);
        u64::from_be_bytes(buf)
    }

    pub fn as_bytes(&self) -> &[u8; SIZE] {
        &self.0
    }
}

impl fmt::Display for Key {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        for b in self.0 {
            write!(f, "{b:02x}")?;
        }
        Ok(())
    }
}

impl fmt::Debug for Key {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "Key({self})")
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn empty_blob_matches_go() {
        let k = Key::new(Type::Blob, 0, &[]);
        assert_eq!(
            k.to_string(),
            "0000af1349b9f5f9a1a6a0404dea36dcc9499bcb25c9adc112b7cc9a93cae41f"
        );
        assert_eq!(k.length(), 0);
        assert_eq!(k.type_(), Type::Blob);
    }

    #[test]
    fn blob_100_x_matches_go() {
        let k = Key::new(Type::Blob, 100, b"x");
        assert_eq!(
            k.to_string(),
            "00643ae7d805f6789a6402acb70ad4096a85a56bf6804eaf25c0493ac697548d"
        );
    }

    #[test]
    fn length_sizes() {
        for (length, ls) in [
            (0u64, 1usize),
            (255, 1),
            (256, 2),
            (65536, 3),
            (1 << 32, 5),
            (u64::MAX, 8),
        ] {
            let k = Key::new_from_hash(Type::FileNode, length, [7u8; SIZE]);
            assert_eq!(k.length_size(), ls);
            assert_eq!(k.length(), length);
            assert_eq!(Key::parse(k.as_bytes()).unwrap(), k);
        }
    }

    #[test]
    fn parse_rejects_bad_keys() {
        assert!(Key::parse(&[0u8; 31]).is_err());
        let mut k = [0u8; SIZE];
        k[0] = 0x08;
        assert!(Key::parse(&k).is_err());
        k[0] = 5 << 4;
        k[1] = 1;
        assert!(Key::parse(&k).is_err());
        k[0] = 0x01;
        k[1] = 0;
        k[2] = 5;
        assert!(Key::parse(&k).is_err());
        assert!(Key::parse(&[0u8; SIZE]).is_ok());
    }

    /// `tests/fixtures/amber/keys.json` from core-rs (Go-generated).
    #[test]
    fn golden_keys() {
        let text = std::fs::read_to_string(concat!(
            env!("CARGO_MANIFEST_DIR"),
            "/tests/fixtures/amber/keys.json"
        ))
        .unwrap();
        let v: serde_json::Value = serde_json::from_str(&text).unwrap();
        let mut n = 0;
        for case in v["cases"].as_array().unwrap() {
            let t = Type::from_u8(case["type"].as_u64().unwrap() as u8).unwrap();
            let length: u64 = case["length"].as_str().unwrap().parse().unwrap();
            let payload = crate::amber::cdc::tests::payload(&case["payload"]);
            let k = Key::new(t, length, &payload);
            assert_eq!(k.to_string(), case["key"].as_str().unwrap());
            n += 1;
        }
        assert!(n > 10);
    }
}
