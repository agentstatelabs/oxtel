//! Trace and span identifiers.
//!
//! Both are random, never all zeros, and print as lowercase hex, as W3C Trace
//! Context and OTLP expect.

use std::fmt;

/// A 16-byte trace id. All zeros is invalid.
#[derive(Clone, Copy, PartialEq, Eq, Hash, PartialOrd, Ord, Default)]
pub struct TraceId([u8; 16]);

/// An 8-byte span id. All zeros is invalid.
#[derive(Clone, Copy, PartialEq, Eq, Hash, PartialOrd, Ord, Default)]
pub struct SpanId([u8; 8]);

impl TraceId {
    /// The all-zeros id, which marks "no trace".
    pub const INVALID: TraceId = TraceId([0; 16]);

    /// A new random id. Its lower 56 bits are uniformly random, as the W3C
    /// `random` trace flag and ratio sampling require.
    pub fn random() -> Self {
        loop {
            let id = fastrand::u128(..);
            if id != 0 {
                return TraceId(id.to_be_bytes());
            }
        }
    }

    /// The id from its 16 bytes.
    pub const fn from_bytes(bytes: [u8; 16]) -> Self {
        TraceId(bytes)
    }

    /// The id's 16 bytes.
    pub const fn to_bytes(self) -> [u8; 16] {
        self.0
    }

    /// Whether the id is anything but all zeros.
    pub fn is_valid(&self) -> bool {
        self.0 != [0; 16]
    }

    /// Parse 32 lowercase hex digits. Uppercase is rejected, as W3C Trace
    /// Context requires.
    pub fn from_hex(hex: &str) -> Option<Self> {
        let mut bytes = [0u8; 16];
        decode_lower_hex(hex, &mut bytes)?;
        Some(TraceId(bytes))
    }

    /// The low 56 bits, the part the W3C `random` flag promises is random.
    pub(crate) fn randomness(&self) -> u64 {
        let mut low = [0u8; 8];
        low[1..].copy_from_slice(&self.0[9..]);
        u64::from_be_bytes(low)
    }
}

impl SpanId {
    /// The all-zeros id, which marks "no span".
    pub const INVALID: SpanId = SpanId([0; 8]);

    /// A new random id.
    pub fn random() -> Self {
        loop {
            let id = fastrand::u64(..);
            if id != 0 {
                return SpanId(id.to_be_bytes());
            }
        }
    }

    /// The id from its 8 bytes.
    pub const fn from_bytes(bytes: [u8; 8]) -> Self {
        SpanId(bytes)
    }

    /// The id's 8 bytes.
    pub const fn to_bytes(self) -> [u8; 8] {
        self.0
    }

    /// Whether the id is anything but all zeros.
    pub fn is_valid(&self) -> bool {
        self.0 != [0; 8]
    }

    /// Parse 16 lowercase hex digits.
    pub fn from_hex(hex: &str) -> Option<Self> {
        let mut bytes = [0u8; 8];
        decode_lower_hex(hex, &mut bytes)?;
        Some(SpanId(bytes))
    }
}

fn decode_lower_hex(hex: &str, out: &mut [u8]) -> Option<()> {
    let digits = hex.as_bytes();
    if digits.len() != out.len() * 2 {
        return None;
    }
    let nibble = |c: u8| match c {
        b'0'..=b'9' => Some(c - b'0'),
        b'a'..=b'f' => Some(c - b'a' + 10),
        _ => None,
    };
    let (pairs, _) = digits.as_chunks::<2>();
    for (byte, [high, low]) in out.iter_mut().zip(pairs) {
        *byte = (nibble(*high)? << 4) | nibble(*low)?;
    }
    Some(())
}

fn write_hex(f: &mut fmt::Formatter<'_>, bytes: &[u8]) -> fmt::Result {
    for b in bytes {
        write!(f, "{b:02x}")?;
    }
    Ok(())
}

impl fmt::Display for TraceId {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write_hex(f, &self.0)
    }
}

impl fmt::Debug for TraceId {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "TraceId({self})")
    }
}

impl fmt::Display for SpanId {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write_hex(f, &self.0)
    }
}

impl fmt::Debug for SpanId {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "SpanId({self})")
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn hex_round_trips_and_rejects_uppercase() {
        let id = TraceId::from_hex("4bf92f3577b34da6a3ce929d0e0e4736").unwrap();
        assert_eq!(id.to_string(), "4bf92f3577b34da6a3ce929d0e0e4736");
        assert!(TraceId::from_hex("4BF92F3577B34DA6A3CE929D0E0E4736").is_none());
        assert!(TraceId::from_hex("4bf92f3577b34da6a3ce929d0e0e473").is_none());
        assert!(SpanId::from_hex("00f067aa0ba902b7").is_some());
        assert!(SpanId::from_hex("00f067aa0ba902bg").is_none());
    }

    #[test]
    fn random_ids_are_valid() {
        for _ in 0..1000 {
            assert!(TraceId::random().is_valid());
            assert!(SpanId::random().is_valid());
        }
        assert!(!TraceId::INVALID.is_valid());
        assert!(!SpanId::INVALID.is_valid());
    }

    #[test]
    fn randomness_is_the_low_56_bits() {
        let id = TraceId::from_hex("ffffffffffffffffff00000000000001").unwrap();
        assert_eq!(id.randomness(), 1);
        let id = TraceId::from_hex("0000000000000000ffffffffffffffff").unwrap();
        assert_eq!(id.randomness(), (1 << 56) - 1);
    }
}
