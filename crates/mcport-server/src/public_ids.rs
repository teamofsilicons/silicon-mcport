//! Public identifiers are a permutation of each complete base62 width.
//! The cursor is arbitrary-width, so growth is unrelated to machine integers.
//! These identifiers are locators, never authentication credentials.
use crate::error::{Error, Result};
use sha2::{Digest, Sha256};

const ALPHABET: &[u8; 62] = b"abcdefghijklmnopqrstuvwxyz0123456789ABCDEFGHIJKLMNOPQRSTUVWXYZ";
pub const INITIAL_CURSOR: &str = "aaa";

/// Keep telemetry's UUID contract separate from public locator formatting.
pub fn correlation_id(public_id: &str) -> uuid::Uuid {
    if let Ok(legacy) = uuid::Uuid::parse_str(public_id) {
        return legacy;
    }
    let mut hash = Sha256::new();
    hash.update(b"mcport-invocation-telemetry-v1:");
    hash.update(public_id.as_bytes());
    let digest = hash.finalize();
    uuid::Builder::from_custom_bytes(digest[..16].try_into().unwrap()).into_uuid()
}

pub struct Cursor(Vec<u8>);
impl Cursor {
    pub fn parse(value: &str) -> Result<Self> {
        if value.len() < 3 {
            return Err(Error::internal());
        }
        value
            .bytes()
            .rev()
            .map(|byte| {
                ALPHABET
                    .iter()
                    .position(|&candidate| candidate == byte)
                    .map(|digit| digit as u8)
                    .ok_or_else(Error::internal)
            })
            .collect::<Result<Vec<_>>>()
            .map(Self)
    }
    pub fn encoded(&self) -> String {
        encode(&self.0)
    }
    pub fn next(&mut self) {
        for digit in &mut self.0 {
            if *digit < 61 {
                *digit += 1;
                return;
            }
            *digit = 0;
        }
        self.0.push(0);
    }
    pub fn public_id(&self, seed: &[u8; 32]) -> String {
        Permutation::new(seed, self.0.len()).apply(self)
    }
}

fn encode(digits: &[u8]) -> String {
    digits
        .iter()
        .rev()
        .map(|digit| ALPHABET[*digit as usize] as char)
        .collect()
}

/// y = a*x+b modulo 62^width is bijective when a is coprime to 62.
/// Seeded offsets and multipliers avoid displaying the underlying counter.
/// This is deliberately not an unguessability or cryptographic secrecy claim.
struct Permutation {
    multiplier: u32,
    offset: Vec<u8>,
}
impl Permutation {
    fn new(seed: &[u8; 32], width: usize) -> Self {
        let mut hash = Sha256::new();
        hash.update(b"mcport-public-ids-v1");
        hash.update(seed);
        hash.update(width.to_string().as_bytes());
        let digest = hash.finalize();
        let mut multiplier = u32::from_be_bytes(digest[..4].try_into().unwrap()) | 1;
        if multiplier.is_multiple_of(31) {
            multiplier -= 2;
        }
        let offset = (0..width)
            .map(|position| {
                let mut hash = Sha256::new();
                hash.update(digest);
                hash.update(position.to_string().as_bytes());
                hash.finalize()[0] % 62
            })
            .collect();
        Self { multiplier, offset }
    }
    fn apply(&self, cursor: &Cursor) -> String {
        let mut carry = 0u64;
        let digits: Vec<u8> = cursor
            .0
            .iter()
            .zip(&self.offset)
            .map(|(&digit, &offset)| {
                let value =
                    u64::from(digit) * u64::from(self.multiplier) + u64::from(offset) + carry;
                carry = value / 62;
                (value % 62) as u8
            })
            .collect();
        encode(&digits)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::collections::HashSet;

    #[test]
    fn every_three_character_value_is_used_once_before_width_grows() {
        let mut cursor = Cursor::parse(INITIAL_CURSOR).unwrap();
        let permutation = Permutation::new(&[7; 32], 3);
        let mut issued = HashSet::new();
        for _ in 0..62usize.pow(3) {
            let id = permutation.apply(&cursor);
            assert_eq!(id.len(), 3);
            assert!(id.bytes().all(|byte| byte.is_ascii_alphanumeric()));
            assert!(issued.insert(id));
            cursor.next();
        }
        assert_eq!(issued.len(), 238_328);
        assert_eq!(cursor.encoded(), "aaaa");
        assert_eq!(cursor.public_id(&[7; 32]).len(), 4);
    }

    #[test]
    fn width_growth_is_not_limited_to_four_or_a_machine_integer() {
        for width in [4, 5, 32, 100] {
            let mut cursor = Cursor::parse(&"Z".repeat(width)).unwrap();
            let previous = cursor.public_id(&[8; 32]);
            assert_eq!(previous.len(), width);
            cursor.next();
            assert_eq!(cursor.encoded(), "a".repeat(width + 1));
            assert_eq!(cursor.public_id(&[8; 32]).len(), width + 1);
        }
    }

    #[test]
    fn telemetry_keeps_legacy_uuids_and_stable_distinct_compact_correlations() {
        let legacy = uuid::Uuid::new_v4();
        assert_eq!(correlation_id(&legacy.to_string()), legacy);
        assert_eq!(correlation_id("aB3"), correlation_id("aB3"));
        assert_ne!(correlation_id("aB3"), correlation_id("aB4"));
        assert_ne!(correlation_id("aB3"), correlation_id("aaB3"));
        assert_eq!(correlation_id("aB3").get_version_num(), 8);
    }
}
