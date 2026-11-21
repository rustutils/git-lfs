//! Types shared by the batch and locking APIs.

use serde::{Deserialize, Serialize};

/// The server ref that a request's objects or locks belong to.
///
/// Servers can use it to authorize by ref. See "Ref Property" in the
/// [batch spec][batch].
///
/// [batch]: https://gitlab.com/rustutils/git-lfs/-/blob/master/docs/api/batch.md
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct Ref {
    /// Fully qualified ref name, such as `refs/heads/main`.
    pub name: String,
}

impl Ref {
    /// Build a ref from a fully qualified ref name.
    pub fn new(name: impl Into<String>) -> Self {
        Self { name: name.into() }
    }
}

/// The user who holds a lock.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct Owner {
    /// The user's name, as the server reports it.
    pub name: String,
}

/// A file lock.
///
/// Fields stay in this order because the `--json` output of the lock
/// commands serializes this type, and upstream's shell tests match it
/// literally.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct Lock {
    /// Server-assigned ID.
    pub id: String,
    /// Locked path, relative to the repository root.
    pub path: String,
    /// Who holds the lock. Optional in the spec.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub owner: Option<Owner>,
    /// When the lock was created, as an RFC 3339 timestamp.
    ///
    /// Kept as the server's string, unparsed.
    pub locked_at: String,
}
