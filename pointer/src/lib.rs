//! Parse and encode Git LFS pointer files.
//!
//! A [`Pointer`] records a file's [`Oid`] (its SHA-256 hash), its size, and
//! any [`Extension`]s that transformed it. The [spec] has the full format.
//!
//! [`Pointer::parse`] reads a pointer from a Git blob, and [`Pointer::encode`]
//! writes one. Parsing accepts some variations that encoding never produces:
//! CRLF line endings, whitespace at the start or end of the input, extensions
//! out of order, and older version URLs. [`Pointer::canonical`] says whether
//! the input was already in canonical form. If it wasn't, re-encoding changes
//! the bytes and so the blob's Git hash. `git lfs fsck` reports such
//! pointers.
//!
//! A parse can fail in two ways. [`DecodeError::NotAPointer`] means the input
//! is ordinary content, and [`DecodeError::Malformed`] means it looks like a
//! pointer but is invalid.
//!
//! ```
//! use git_lfs_pointer::{Oid, Pointer};
//!
//! let oid: Oid = "4d7a214614ab2935c943f9e0ff69d22eadbb8f32b1258daaa5e2ca24d17e2393"
//!     .parse()
//!     .unwrap();
//! let pointer = Pointer::new(oid, 12345);
//!
//! let encoded = pointer.encode();
//! let parsed = Pointer::parse(encoded.as_bytes()).unwrap();
//! assert_eq!(parsed.oid, oid);
//! assert_eq!(parsed.size, 12345);
//! assert!(parsed.canonical);
//! ```
//!
//! [spec]: https://gitlab.com/rustutils/git-lfs/-/blob/master/docs/spec.md

mod oid;

pub use oid::{EMPTY_HEX, Oid, OidParseError};

/// The version URL we always emit. Older aliases parse but re-encode to this.
pub const VERSION_LATEST: &str = "https://git-lfs.github.com/spec/v1";

/// The size limit for pointer files.
///
/// A pointer must be smaller than this, so input this size or larger is never a
/// pointer.
pub const MAX_POINTER_SIZE: usize = 1024;

/// Version URLs accepted when parsing.
const VERSION_ALIASES: &[&str] = &[
    "http://git-media.io/v/2",            // alpha
    "https://hawser.github.com/spec/v1",  // pre-release
    "https://git-lfs.github.com/spec/v1", // current
];

/// A Git LFS pointer.
///
/// A pointer with size 0 is the *empty pointer*. It represents an empty file
/// and encodes to an empty string. Its OID is normally [`Oid::EMPTY`].
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Pointer {
    /// SHA-256 of the file's content.
    pub oid: Oid,
    /// Size of the file in bytes.
    pub size: u64,
    /// Extensions, sorted by priority.
    pub extensions: Vec<Extension>,
    /// Whether the parsed input was exactly the canonical encoding.
    ///
    /// Always `true` for pointers built with [`Pointer::new`] or
    /// [`Pointer::empty`].
    pub canonical: bool,
}

/// A pointer extension.
///
/// An extension is a program that transformed the file's content before it
/// was stored. The [extensions spec] explains how they're chained.
///
/// [extensions spec]: https://gitlab.com/rustutils/git-lfs/-/blob/master/docs/extensions.md
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Extension {
    /// Extension name: ASCII letters, digits, and `_`.
    pub name: String,
    /// Priority from 0 to 9. Lower priorities run first when cleaning.
    pub priority: u8,
    /// SHA-256 of this extension's input when cleaning.
    pub oid: Oid,
}

impl Pointer {
    /// Build a pointer with no extensions.
    ///
    /// With a size of 0, this is the empty pointer.
    pub fn new(oid: Oid, size: u64) -> Self {
        Self {
            oid,
            size,
            extensions: Vec::new(),
            canonical: true,
        }
    }

    /// The empty pointer, which represents an empty file.
    ///
    /// Parsing empty input returns this.
    pub fn empty() -> Self {
        Self {
            oid: Oid::EMPTY,
            size: 0,
            extensions: Vec::new(),
            canonical: true,
        }
    }

    /// Whether this is the empty pointer (size 0).
    pub fn is_empty(&self) -> bool {
        self.size == 0
    }

    /// Encode in canonical form.
    ///
    /// The empty pointer encodes to an empty string. The version line is
    /// always [`VERSION_LATEST`].
    pub fn encode(&self) -> String {
        use std::fmt::Write as _;
        if self.size == 0 {
            return String::new();
        }
        let mut exts: Vec<&Extension> = self.extensions.iter().collect();
        exts.sort_by_key(|e| e.priority);

        let mut out = String::with_capacity(160 + 80 * exts.len());
        writeln!(out, "version {VERSION_LATEST}").unwrap();
        for ext in exts {
            writeln!(out, "ext-{}-{} sha256:{}", ext.priority, ext.name, ext.oid).unwrap();
        }
        writeln!(out, "oid sha256:{}", self.oid).unwrap();
        writeln!(out, "size {}", self.size).unwrap();
        out
    }

    /// Parse a pointer from the contents of a blob.
    ///
    /// Empty input parses as [`Pointer::empty`].
    pub fn parse(input: &[u8]) -> Result<Self, DecodeError> {
        if input.is_empty() {
            return Ok(Self::empty());
        }
        if input.len() >= MAX_POINTER_SIZE {
            return Err(DecodeError::NotAPointer(NotAPointerReason::TooLarge {
                size: input.len(),
            }));
        }
        let text = std::str::from_utf8(input)
            .map_err(|_| DecodeError::NotAPointer(NotAPointerReason::NotUtf8))?;
        if !contains_spec_marker(text) {
            return Err(DecodeError::NotAPointer(NotAPointerReason::MissingHeader));
        }

        let mut pointer = parse_lines(text.trim())?;
        pointer.canonical = pointer.encode().as_bytes() == input;
        Ok(pointer)
    }
}

fn contains_spec_marker(text: &str) -> bool {
    text.contains("git-lfs") || text.contains("git-media") || text.contains("hawser")
}

fn parse_lines(text: &str) -> Result<Pointer, DecodeError> {
    const REQUIRED: [&str; 3] = ["version", "oid", "size"];
    let mut filled: [Option<&str>; 3] = [None, None, None];
    let mut consumed = 0usize;
    let mut extensions: Vec<Extension> = Vec::new();

    for (line_no, raw_line) in text.split('\n').enumerate() {
        // Upstream's bufio.Scanner accepts CRLF, so we do too.
        let line = raw_line.strip_suffix('\r').unwrap_or(raw_line);
        if line.is_empty() {
            continue;
        }

        let (key, value) = line.split_once(' ').ok_or(DecodeError::NotAPointer(
            NotAPointerReason::MalformedLine { line: line_no + 1 },
        ))?;

        if consumed == REQUIRED.len() {
            return Err(DecodeError::NotAPointer(NotAPointerReason::ExtraLine {
                line: line_no + 1,
                content: line.into(),
            }));
        }

        let expected = REQUIRED[consumed];
        if key == expected {
            filled[consumed] = Some(value);
            consumed += 1;
            continue;
        }

        // Mismatch: try to parse as an extension.
        if let Some((priority, name)) = parse_extension_key(key) {
            let ext_oid = parse_oid_value(value).map_err(DecodeError::Malformed)?;
            extensions.push(Extension {
                name: name.to_owned(),
                priority,
                oid: ext_oid,
            });
            continue;
        }

        // An unknown key before the version line means this isn't a pointer,
        // as in upstream's StandardizeBadPointerError. After it, the pointer
        // is malformed.
        return Err(if expected == "version" {
            DecodeError::NotAPointer(NotAPointerReason::NotVersionFirst { got: key.into() })
        } else {
            DecodeError::Malformed(MalformedReason::UnexpectedKey {
                expected,
                got: key.into(),
            })
        });
    }

    let version = filled[0].ok_or(DecodeError::NotAPointer(NotAPointerReason::MissingVersion))?;
    if !VERSION_ALIASES.contains(&version) {
        return Err(DecodeError::Malformed(MalformedReason::InvalidVersion(
            version.into(),
        )));
    }

    let oid_value =
        filled[1].ok_or(DecodeError::Malformed(MalformedReason::MissingField("oid")))?;
    let oid = parse_oid_value(oid_value).map_err(DecodeError::Malformed)?;

    let size_value = filled[2].ok_or(DecodeError::Malformed(MalformedReason::MissingField(
        "size",
    )))?;
    let size = parse_size(size_value).map_err(DecodeError::Malformed)?;

    extensions.sort_by_key(|e| e.priority);
    for w in extensions.windows(2) {
        if w[0].priority == w[1].priority {
            return Err(DecodeError::Malformed(
                MalformedReason::DuplicateExtensionPriority(w[0].priority),
            ));
        }
    }

    Ok(Pointer {
        oid,
        size,
        extensions,
        canonical: true, // overwritten by Pointer::parse
    })
}

fn parse_oid_value(value: &str) -> Result<Oid, MalformedReason> {
    let (oid_type, hash) = value
        .split_once(':')
        .ok_or_else(|| MalformedReason::MalformedOidValue(value.into()))?;
    if oid_type != "sha256" {
        return Err(MalformedReason::UnsupportedOidType(oid_type.into()));
    }
    Oid::from_hex(hash).map_err(MalformedReason::InvalidOidHash)
}

fn parse_size(value: &str) -> Result<u64, MalformedReason> {
    // Parsing as u64 already rejects signs, whitespace, and non-digits.
    value
        .parse::<u64>()
        .map_err(|_| MalformedReason::InvalidSize(value.into()))
}

/// Split an `ext-<priority>-<name>` key into its priority and name.
fn parse_extension_key(key: &str) -> Option<(u8, &str)> {
    let rest = key.strip_prefix("ext-")?;
    let bytes = rest.as_bytes();
    if bytes.len() < 3 {
        return None;
    }
    if !bytes[0].is_ascii_digit() || bytes[1] != b'-' {
        return None;
    }
    let name = &rest[2..];
    if !name.bytes().all(|b| b.is_ascii_alphanumeric() || b == b'_') {
        return None;
    }
    Some((bytes[0] - b'0', name))
}

/// Why [`Pointer::parse`] failed.
#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
pub enum DecodeError {
    /// The input isn't a pointer. Treat it as ordinary content.
    #[error("not a git-lfs pointer: {0}")]
    NotAPointer(NotAPointerReason),
    /// The input looks like a pointer but is invalid. Report it as an error.
    #[error("malformed git-lfs pointer: {0}")]
    Malformed(MalformedReason),
}

impl DecodeError {
    /// Whether this is [`DecodeError::NotAPointer`].
    pub fn is_not_a_pointer(&self) -> bool {
        matches!(self, DecodeError::NotAPointer(_))
    }
}

/// Why input isn't a pointer.
#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
pub enum NotAPointerReason {
    /// The input is [`MAX_POINTER_SIZE`] bytes or larger.
    #[error("size {size} bytes is not below the {MAX_POINTER_SIZE}-byte cutoff")]
    TooLarge { size: usize },
    /// The input isn't valid UTF-8.
    #[error("input is not valid UTF-8")]
    NotUtf8,
    /// The input doesn't mention git-lfs or its older names.
    #[error("missing git-lfs spec marker")]
    MissingHeader,
    /// A line (counting from 1) has no space between key and value.
    #[error("line {line} has no key/value separator")]
    MalformedLine { line: usize },
    /// There's no version line.
    #[error("missing version line")]
    MissingVersion,
    /// The first line isn't the version line.
    #[error("first key is {got:?}, expected version")]
    NotVersionFirst { got: String },
    /// A line (counting from 1) follows the size line.
    #[error("extra content on line {line}: {content:?}")]
    ExtraLine { line: usize, content: String },
}

/// Why a pointer is malformed.
#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
pub enum MalformedReason {
    /// The version URL isn't one we recognize.
    #[error("unrecognized version: {0:?}")]
    InvalidVersion(String),
    /// A line's key is out of place.
    #[error("expected key {expected:?}, got {got:?}")]
    UnexpectedKey { expected: &'static str, got: String },
    /// The `oid` or `size` line is missing.
    #[error("missing required {0:?} line")]
    MissingField(&'static str),
    /// An OID isn't in `<type>:<hash>` form.
    #[error("oid value {0:?} is not in the form <type>:<hash>")]
    MalformedOidValue(String),
    /// An OID's type isn't `sha256`, the only supported type.
    #[error("unsupported oid type {0:?}; only sha256 is supported")]
    UnsupportedOidType(String),
    /// An OID's hash is invalid.
    #[error("invalid oid hash: {0}")]
    InvalidOidHash(#[source] OidParseError),
    /// The size isn't a non-negative integer.
    #[error("size value {0:?} is not a non-negative integer")]
    InvalidSize(String),
    /// Two extensions have the same priority.
    #[error("duplicate extension priority {0}")]
    DuplicateExtensionPriority(u8),
}

#[cfg(test)]
mod tests {
    use super::*;

    fn sha(hex: &str) -> Oid {
        Oid::from_hex(hex).unwrap()
    }

    const SAMPLE_OID_HEX: &str = "4d7a214614ab2935c943f9e0ff69d22eadbb8f32b1258daaa5e2ca24d17e2393";

    // ---------- encode ----------

    #[test]
    fn encode_simple() {
        let p = Pointer::new(sha(SAMPLE_OID_HEX), 12345);
        let expected =
            format!("version {VERSION_LATEST}\noid sha256:{SAMPLE_OID_HEX}\nsize 12345\n");
        assert_eq!(p.encode(), expected);
    }

    #[test]
    fn encode_empty() {
        // Per spec, the empty pointer encodes to the empty string.
        assert_eq!(Pointer::empty().encode(), "");
        // Any pointer with size 0 also encodes to "" (matches upstream).
        let p = Pointer::new(sha(SAMPLE_OID_HEX), 0);
        assert_eq!(p.encode(), "");
    }

    #[test]
    fn encode_extensions_sorted_on_output() {
        let exts = vec![
            Extension {
                name: "baz".into(),
                priority: 2,
                oid: sha("aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa"),
            },
            Extension {
                name: "foo".into(),
                priority: 0,
                oid: sha("ffffffffffffffffffffffffffffffffffffffffffffffffffffffffffffffff"),
            },
            Extension {
                name: "bar".into(),
                priority: 1,
                oid: sha("bbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbb"),
            },
        ];
        let p = Pointer {
            oid: sha(SAMPLE_OID_HEX),
            size: 12345,
            extensions: exts,
            canonical: true,
        };
        let expected = format!(
            "version {VERSION_LATEST}\n\
             ext-0-foo sha256:ffffffffffffffffffffffffffffffffffffffffffffffffffffffffffffffff\n\
             ext-1-bar sha256:bbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbb\n\
             ext-2-baz sha256:aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa\n\
             oid sha256:{SAMPLE_OID_HEX}\n\
             size 12345\n",
        );
        assert_eq!(p.encode(), expected);
    }

    // ---------- parse: happy paths ----------

    #[test]
    fn parse_standard() {
        let input = format!("version {VERSION_LATEST}\noid sha256:{SAMPLE_OID_HEX}\nsize 12345\n");
        let p = Pointer::parse(input.as_bytes()).unwrap();
        assert_eq!(p.oid, sha(SAMPLE_OID_HEX));
        assert_eq!(p.size, 12345);
        assert!(p.extensions.is_empty());
        assert!(p.canonical);
    }

    #[test]
    fn parse_empty_input_is_empty_pointer() {
        let p = Pointer::parse(b"").unwrap();
        assert_eq!(p, Pointer::empty());
        assert!(p.canonical);
    }

    #[test]
    fn parse_extensions_sorted() {
        let input = format!(
            "version {VERSION_LATEST}\n\
             ext-0-foo sha256:ffffffffffffffffffffffffffffffffffffffffffffffffffffffffffffffff\n\
             ext-1-bar sha256:bbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbb\n\
             ext-2-baz sha256:aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa\n\
             oid sha256:{SAMPLE_OID_HEX}\n\
             size 12345\n",
        );
        let p = Pointer::parse(input.as_bytes()).unwrap();
        assert_eq!(p.extensions.len(), 3);
        assert_eq!(p.extensions[0].name, "foo");
        assert_eq!(p.extensions[0].priority, 0);
        assert_eq!(p.extensions[1].name, "bar");
        assert_eq!(p.extensions[2].name, "baz");
        assert!(p.canonical);
    }

    #[test]
    fn parse_unsorted_extensions_sorts_and_marks_noncanonical() {
        // Same content, but ext-2 listed first.
        let input = format!(
            "version {VERSION_LATEST}\n\
             ext-2-baz sha256:aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa\n\
             ext-0-foo sha256:ffffffffffffffffffffffffffffffffffffffffffffffffffffffffffffffff\n\
             ext-1-bar sha256:bbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbb\n\
             oid sha256:{SAMPLE_OID_HEX}\n\
             size 12345\n",
        );
        let p = Pointer::parse(input.as_bytes()).unwrap();
        assert_eq!(p.extensions[0].priority, 0);
        assert_eq!(p.extensions[1].priority, 1);
        assert_eq!(p.extensions[2].priority, 2);
        assert!(!p.canonical);
    }

    #[test]
    fn parse_pre_release_version_alias() {
        let input = format!(
            "version https://hawser.github.com/spec/v1\noid sha256:{SAMPLE_OID_HEX}\nsize 12345\n"
        );
        let p = Pointer::parse(input.as_bytes()).unwrap();
        assert_eq!(p.size, 12345);
        // Re-encoding rewrites version to latest, so input is NOT canonical.
        assert!(!p.canonical);
        assert!(
            p.encode()
                .starts_with(&format!("version {VERSION_LATEST}\n"))
        );
    }

    #[test]
    fn parse_round_trip() {
        let p = Pointer::new(sha(SAMPLE_OID_HEX), 12345);
        let encoded = p.encode();
        let parsed = Pointer::parse(encoded.as_bytes()).unwrap();
        assert_eq!(parsed.oid, p.oid);
        assert_eq!(parsed.size, p.size);
        assert!(parsed.canonical);
    }

    // ---------- canonical bytes ----------

    #[test]
    fn canonical_examples() {
        // Standard form, with trailing \n.
        let s = format!("version {VERSION_LATEST}\noid sha256:{SAMPLE_OID_HEX}\nsize 12345\n");
        assert!(Pointer::parse(s.as_bytes()).unwrap().canonical);

        // Empty input.
        assert!(Pointer::parse(b"").unwrap().canonical);
    }

    #[test]
    fn non_canonical_examples() {
        let cases: &[&str] = &[
            // missing trailing newline
            "version https://git-lfs.github.com/spec/v1\noid sha256:4d7a214614ab2935c943f9e0ff69d22eadbb8f32b1258daaa5e2ca24d17e2393\nsize 12345",
            // CRLF line endings
            "version https://git-lfs.github.com/spec/v1\r\noid sha256:4d7a214614ab2935c943f9e0ff69d22eadbb8f32b1258daaa5e2ca24d17e2393\r\nsize 12345\r\n",
            // trailing whitespace on a line
            "version https://git-lfs.github.com/spec/v1\noid sha256:4d7a214614ab2935c943f9e0ff69d22eadbb8f32b1258daaa5e2ca24d17e2393\nsize 12345   \n",
        ];
        for case in cases {
            let p = Pointer::parse(case.as_bytes())
                .unwrap_or_else(|e| panic!("failed to parse {case:?}: {e}"));
            assert!(!p.canonical, "expected non-canonical for {case:?}");
        }
    }

    // ---------- parse: NotAPointer ----------

    #[test]
    fn tiny_non_pointer_is_not_a_pointer() {
        let err = Pointer::parse(b"this is not a git-lfs file!").unwrap_err();
        assert!(err.is_not_a_pointer(), "expected NotAPointer, got {err:?}");
    }

    #[test]
    fn header_only_is_not_a_pointer() {
        // Mentions git-media so passes the marker check, but no key/value.
        let err = Pointer::parse(b"# git-media").unwrap_err();
        assert!(err.is_not_a_pointer(), "expected NotAPointer, got {err:?}");
    }

    #[test]
    fn oversized_input_is_not_a_pointer() {
        let big = vec![b'x'; MAX_POINTER_SIZE + 1];
        let err = Pointer::parse(&big).unwrap_err();
        assert!(matches!(
            err,
            DecodeError::NotAPointer(NotAPointerReason::TooLarge { .. })
        ));
    }

    #[test]
    fn exactly_max_size_is_not_a_pointer() {
        // Spec: pointer files must be *less than* 1024 bytes. At-cutoff is too large.
        let exact = vec![b'x'; MAX_POINTER_SIZE];
        let err = Pointer::parse(&exact).unwrap_err();
        assert!(matches!(
            err,
            DecodeError::NotAPointer(NotAPointerReason::TooLarge { .. })
        ));
    }

    #[test]
    fn equals_separator_is_not_a_pointer() {
        // From upstream's TestDecodeInvalid: bad `key value` format using '='.
        let s = "version=https://git-lfs.github.com/spec/v1\n\
                 oid=sha256:4d7a214614ab2935c943f9e0ff69d22eadbb8f32b1258daaa5e2ca24d17e2393\n\
                 size=fif";
        let err = Pointer::parse(s.as_bytes()).unwrap_err();
        assert!(err.is_not_a_pointer());
    }

    #[test]
    fn no_marker_is_not_a_pointer() {
        let err = Pointer::parse(b"version=http://wat.io/v/2\noid=foo\nsize=fif").unwrap_err();
        assert!(matches!(
            err,
            DecodeError::NotAPointer(NotAPointerReason::MissingHeader)
        ));
    }

    #[test]
    fn missing_version_first_is_not_a_pointer() {
        // OID line first, no version. From upstream's "no version" case.
        let s = format!("oid sha256:{SAMPLE_OID_HEX}\nsize 12345\n");
        let err = Pointer::parse(s.as_bytes()).unwrap_err();
        assert!(err.is_not_a_pointer(), "got {err:?}");
    }

    #[test]
    fn extra_line_after_size_is_not_a_pointer() {
        let s =
            format!("version {VERSION_LATEST}\noid sha256:{SAMPLE_OID_HEX}\nsize 12345\nwat wat\n");
        let err = Pointer::parse(s.as_bytes()).unwrap_err();
        assert!(matches!(
            err,
            DecodeError::NotAPointer(NotAPointerReason::ExtraLine { .. })
        ));
    }

    // ---------- parse: Malformed ----------

    #[test]
    fn invalid_version_is_malformed() {
        // Non-empty version that isn't an alias.
        let s = format!(
            "version http://git-media.io/v/whatever\noid sha256:{SAMPLE_OID_HEX}\nsize 12345\n"
        );
        let err = Pointer::parse(s.as_bytes()).unwrap_err();
        assert!(matches!(
            err,
            DecodeError::Malformed(MalformedReason::InvalidVersion(_))
        ));
    }

    #[test]
    fn missing_oid_is_malformed() {
        let s = format!("version {VERSION_LATEST}\nsize 12345\n");
        let err = Pointer::parse(s.as_bytes()).unwrap_err();
        assert!(matches!(err, DecodeError::Malformed(_)));
    }

    #[test]
    fn missing_size_is_malformed() {
        let s = format!("version {VERSION_LATEST}\noid sha256:{SAMPLE_OID_HEX}\n");
        let err = Pointer::parse(s.as_bytes()).unwrap_err();
        assert!(matches!(
            err,
            DecodeError::Malformed(MalformedReason::MissingField("size"))
        ));
    }

    #[test]
    fn keys_out_of_order_is_malformed() {
        let s = format!("version {VERSION_LATEST}\nsize 12345\noid sha256:{SAMPLE_OID_HEX}\n");
        let err = Pointer::parse(s.as_bytes()).unwrap_err();
        assert!(matches!(
            err,
            DecodeError::Malformed(MalformedReason::UnexpectedKey { .. })
        ));
    }

    #[test]
    fn bad_oid_hex_is_malformed() {
        let s = format!("version {VERSION_LATEST}\noid sha256:boom\nsize 12345\n");
        let err = Pointer::parse(s.as_bytes()).unwrap_err();
        assert!(matches!(
            err,
            DecodeError::Malformed(MalformedReason::InvalidOidHash(_))
        ));
    }

    #[test]
    fn bad_oid_type_is_malformed() {
        let s = format!("version {VERSION_LATEST}\noid shazam:{SAMPLE_OID_HEX}\nsize 12345\n");
        let err = Pointer::parse(s.as_bytes()).unwrap_err();
        assert!(matches!(
            err,
            DecodeError::Malformed(MalformedReason::UnsupportedOidType(_))
        ));
    }

    #[test]
    fn bad_size_is_malformed() {
        let s = format!("version {VERSION_LATEST}\noid sha256:{SAMPLE_OID_HEX}\nsize fif\n");
        let err = Pointer::parse(s.as_bytes()).unwrap_err();
        assert!(matches!(
            err,
            DecodeError::Malformed(MalformedReason::InvalidSize(_))
        ));
    }

    #[test]
    fn negative_size_is_malformed() {
        let s = format!("version {VERSION_LATEST}\noid sha256:{SAMPLE_OID_HEX}\nsize -1\n");
        let err = Pointer::parse(s.as_bytes()).unwrap_err();
        assert!(matches!(
            err,
            DecodeError::Malformed(MalformedReason::InvalidSize(_))
        ));
    }

    #[test]
    fn oid_with_trailing_garbage_is_malformed() {
        let s = format!("version {VERSION_LATEST}\noid sha256:{SAMPLE_OID_HEX}&\nsize 177735\n");
        let err = Pointer::parse(s.as_bytes()).unwrap_err();
        assert!(matches!(
            err,
            DecodeError::Malformed(MalformedReason::InvalidOidHash(_))
        ));
    }

    // ---------- parse: extensions ----------

    #[test]
    fn ext_priority_over_9_is_malformed() {
        // ext-10-foo: priority must be a single digit (matches upstream regex).
        let s = format!(
            "version {VERSION_LATEST}\n\
             ext-10-foo sha256:ffffffffffffffffffffffffffffffffffffffffffffffffffffffffffffffff\n\
             oid sha256:{SAMPLE_OID_HEX}\n\
             size 12345\n",
        );
        let err = Pointer::parse(s.as_bytes()).unwrap_err();
        assert!(matches!(err, DecodeError::Malformed(_)), "got {err:?}");
    }

    #[test]
    fn ext_with_non_digit_priority_is_malformed() {
        let s = format!(
            "version {VERSION_LATEST}\n\
             ext-#-foo sha256:ffffffffffffffffffffffffffffffffffffffffffffffffffffffffffffffff\n\
             oid sha256:{SAMPLE_OID_HEX}\n\
             size 12345\n",
        );
        let err = Pointer::parse(s.as_bytes()).unwrap_err();
        assert!(matches!(err, DecodeError::Malformed(_)), "got {err:?}");
    }

    #[test]
    fn ext_with_non_word_name_is_malformed() {
        let s = format!(
            "version {VERSION_LATEST}\n\
             ext-0-$$$$ sha256:ffffffffffffffffffffffffffffffffffffffffffffffffffffffffffffffff\n\
             oid sha256:{SAMPLE_OID_HEX}\n\
             size 12345\n",
        );
        let err = Pointer::parse(s.as_bytes()).unwrap_err();
        assert!(matches!(err, DecodeError::Malformed(_)), "got {err:?}");
    }

    #[test]
    fn ext_bad_oid_is_malformed() {
        let s = format!(
            "version {VERSION_LATEST}\n\
             ext-0-foo sha256:boom\n\
             oid sha256:{SAMPLE_OID_HEX}\n\
             size 12345\n",
        );
        let err = Pointer::parse(s.as_bytes()).unwrap_err();
        assert!(matches!(
            err,
            DecodeError::Malformed(MalformedReason::InvalidOidHash(_))
        ));
    }

    #[test]
    fn ext_bad_oid_type_is_malformed() {
        let s = format!(
            "version {VERSION_LATEST}\n\
             ext-0-foo boom:ffffffffffffffffffffffffffffffffffffffffffffffffffffffffffffffff\n\
             oid sha256:{SAMPLE_OID_HEX}\n\
             size 12345\n",
        );
        let err = Pointer::parse(s.as_bytes()).unwrap_err();
        assert!(matches!(
            err,
            DecodeError::Malformed(MalformedReason::UnsupportedOidType(_))
        ));
    }

    #[test]
    fn duplicate_ext_priority_is_malformed() {
        let s = format!(
            "version {VERSION_LATEST}\n\
             ext-0-foo sha256:ffffffffffffffffffffffffffffffffffffffffffffffffffffffffffffffff\n\
             ext-0-bar sha256:bbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbb\n\
             oid sha256:{SAMPLE_OID_HEX}\n\
             size 12345\n",
        );
        let err = Pointer::parse(s.as_bytes()).unwrap_err();
        assert!(matches!(
            err,
            DecodeError::Malformed(MalformedReason::DuplicateExtensionPriority(0))
        ));
    }
}
