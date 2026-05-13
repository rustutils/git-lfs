//! Types shared between the batch and locking endpoints.

use serde::{Deserialize, Serialize};

/// A server refspec, used by both batch and locking requests for
/// auth schemes that take the ref into account (added in LFS v2.4).
///
/// See the [batch spec][batch] § "Ref Property".
///
/// [batch]: https://gitlab.com/rustutils/git-lfs/-/blob/master/docs/api/batch.md
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct Ref {
    /// Full refname, e.g. `refs/heads/main`.
    pub name: String,
}

impl Ref {
    /// Build a refspec from a refname.
    pub fn new(name: impl Into<String>) -> Self {
        Self { name: name.into() }
    }
}

/// User identity attached to a lock by the server.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct Owner {
    /// Server-supplied display name of the lock owner.
    pub name: String,
}

/// A lock record returned by the locking API.
///
/// `locked_at` is an uppercase RFC 3339-formatted timestamp with second
/// precision per the spec. Carried as a string; parsing into a typed
/// timestamp is left to callers (avoids pulling in a date/time crate here).
///
/// Field order matters for `--json` output: the upstream test fixtures
/// `grep -E` against literal `{"id":...,"path":...,"owner":...,"locked_at":...}`,
/// and serde's derived `Serialize` follows declaration order.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct Lock {
    /// Server-assigned lock identifier.
    pub id: String,
    /// Repo-relative path that's locked.
    pub path: String,
    /// User who holds the lock. Servers may omit when the requester
    /// lacks permission to see the owner.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub owner: Option<Owner>,
    /// RFC 3339 timestamp when the lock was created.
    pub locked_at: String,
}
