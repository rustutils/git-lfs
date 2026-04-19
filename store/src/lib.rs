//! Content-addressable object store for git-lfs.
//!
//! Manages `.git/lfs/objects/{OID-PATH}` where `{OID-PATH}` is the sharded
//! path `OID[0:2]/OID[2:4]/OID`. See `docs/spec.md`.
