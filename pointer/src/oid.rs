use std::fmt;
use std::str::FromStr;

/// Hex form of the SHA-256 of the empty input. Used as the OID of the empty
/// pointer (which represents an empty file — see `docs/spec.md`).
pub const EMPTY_HEX: &str = "e3b0c44298fc1c149afbf4c8996fb92427ae41e4649b934ca495991b7852b855";

/// A SHA-256 object identifier.
///
/// Stored as the raw 32 bytes; rendered as 64 lowercase hex characters by
/// [`fmt::Display`]. Construction via [`Oid::from_hex`] enforces the spec's
/// strict-lowercase, exactly-64-hex-character format.
#[derive(Clone, Copy, PartialEq, Eq, Hash, PartialOrd, Ord)]
pub struct Oid([u8; 32]);

impl Oid {
    /// SHA-256 of the empty input. The OID of the [empty pointer].
    ///
    /// [empty pointer]: crate::Pointer::empty
    pub const EMPTY: Oid = Oid([
        0xe3, 0xb0, 0xc4, 0x42, 0x98, 0xfc, 0x1c, 0x14, 0x9a, 0xfb, 0xf4, 0xc8, 0x99, 0x6f, 0xb9,
        0x24, 0x27, 0xae, 0x41, 0xe4, 0x64, 0x9b, 0x93, 0x4c, 0xa4, 0x95, 0x99, 0x1b, 0x78, 0x52,
        0xb8, 0x55,
    ]);

    /// Construct an OID from raw 32 hash bytes (e.g. the output of a
    /// streaming SHA-256 hasher).
    pub const fn from_bytes(bytes: [u8; 32]) -> Self {
        Self(bytes)
    }

    /// Parse an OID from its 64-character lowercase hex form.
    pub fn from_hex(s: &str) -> Result<Self, OidParseError> {
        if s.len() != 64 {
            return Err(OidParseError::InvalidLength(s.len()));
        }
        let mut out = [0u8; 32];
        let bytes = s.as_bytes();
        for (i, byte) in out.iter_mut().enumerate() {
            let hi = hex_digit(bytes[i * 2])?;
            let lo = hex_digit(bytes[i * 2 + 1])?;
            *byte = (hi << 4) | lo;
        }
        Ok(Oid(out))
    }

    /// Borrow the raw 32-byte hash.
    pub fn as_bytes(&self) -> &[u8; 32] {
        &self.0
    }
}

fn hex_digit(b: u8) -> Result<u8, OidParseError> {
    match b {
        b'0'..=b'9' => Ok(b - b'0'),
        b'a'..=b'f' => Ok(b - b'a' + 10),
        // Uppercase A-F is rejected on purpose: the spec mandates lowercase.
        _ => Err(OidParseError::InvalidCharacter(b as char)),
    }
}

impl fmt::Display for Oid {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        for byte in &self.0 {
            write!(f, "{byte:02x}")?;
        }
        Ok(())
    }
}

impl fmt::Debug for Oid {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "Oid({self})")
    }
}

impl FromStr for Oid {
    type Err = OidParseError;
    fn from_str(s: &str) -> Result<Self, Self::Err> {
        Oid::from_hex(s)
    }
}

#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
pub enum OidParseError {
    #[error("oid must be 64 hex characters, got {0}")]
    InvalidLength(usize),
    #[error("oid contains invalid character {0:?} (must be lowercase 0-9a-f)")]
    InvalidCharacter(char),
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn empty_const_matches_empty_hex() {
        assert_eq!(Oid::EMPTY, Oid::from_hex(EMPTY_HEX).unwrap());
        assert_eq!(Oid::EMPTY.to_string(), EMPTY_HEX);
    }

    #[test]
    fn round_trip_hex() {
        let hex = "4d7a214614ab2935c943f9e0ff69d22eadbb8f32b1258daaa5e2ca24d17e2393";
        let oid = Oid::from_hex(hex).unwrap();
        assert_eq!(oid.to_string(), hex);
    }

    #[test]
    fn rejects_wrong_length() {
        assert_eq!(Oid::from_hex(""), Err(OidParseError::InvalidLength(0)));
        assert_eq!(Oid::from_hex("abc"), Err(OidParseError::InvalidLength(3)));
        assert_eq!(
            Oid::from_hex(&"a".repeat(63)),
            Err(OidParseError::InvalidLength(63))
        );
        assert_eq!(
            Oid::from_hex(&"a".repeat(65)),
            Err(OidParseError::InvalidLength(65))
        );
    }

    #[test]
    fn rejects_uppercase() {
        // Spec mandates lowercase; uppercase A-F is not accepted.
        let upper = "4D7A214614AB2935C943F9E0FF69D22EADBB8F32B1258DAAA5E2CA24D17E2393";
        assert_eq!(
            Oid::from_hex(upper),
            Err(OidParseError::InvalidCharacter('D'))
        );
    }

    #[test]
    fn rejects_non_hex() {
        let mut bad = "a".repeat(63);
        bad.push('z');
        assert_eq!(
            Oid::from_hex(&bad),
            Err(OidParseError::InvalidCharacter('z'))
        );

        let trailing_amp = "4d7a214614ab2935c943f9e0ff69d22eadbb8f32b1258daaa5e2ca24d17e2393&"; // 65 chars
        assert_eq!(
            Oid::from_hex(trailing_amp),
            Err(OidParseError::InvalidLength(65))
        );
    }

    #[test]
    fn from_str_works() {
        let hex = "4d7a214614ab2935c943f9e0ff69d22eadbb8f32b1258daaa5e2ca24d17e2393";
        let oid: Oid = hex.parse().unwrap();
        assert_eq!(oid.to_string(), hex);
    }
}
