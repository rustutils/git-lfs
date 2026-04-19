//! Types shared between the batch and locking endpoints.

use serde::{Deserialize, Serialize};

/// A server refspec, used by both batch and locking requests for
/// auth schemes that take the ref into account (added in LFS v2.4).
///
/// See `docs/api/batch.md` § "Ref Property".
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct Ref {
    pub name: String,
}

impl Ref {
    pub fn new(name: impl Into<String>) -> Self {
        Self { name: name.into() }
    }
}

/// User identity attached to a lock by the server.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct Owner {
    pub name: String,
}

/// A lock record returned by the locking API.
///
/// `locked_at` is an uppercase RFC 3339-formatted timestamp with second
/// precision per the spec. We carry it as a string — parsing into a typed
/// timestamp is left to callers (avoids pulling in a date/time crate here).
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct Lock {
    pub id: String,
    pub path: String,
    pub locked_at: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub owner: Option<Owner>,
}
