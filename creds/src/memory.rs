//! In-memory credential cache.

use std::collections::HashMap;
use std::io::Write as _;
use std::sync::Mutex;

use crate::helper::{Credentials, Helper, HelperError, HelperOutcome};
use crate::query::Query;
use crate::trace::trace_enabled;

/// In-memory credential cache, keyed on the whole [`Query`].
///
/// Approve stores credentials, fill returns them, and reject drops them.
/// Entries never expire, so the cache suits a short-lived process.
#[derive(Debug, Default)]
pub struct CachingHelper {
    cache: Mutex<HashMap<Query, Credentials>>,
}

impl CachingHelper {
    /// Create an empty cache.
    pub fn new() -> Self {
        Self::default()
    }
}

impl Helper for CachingHelper {
    fn fill(&self, query: &Query) -> Result<Option<Credentials>, HelperError> {
        let hit = self.cache.lock().unwrap().get(query).cloned();
        if hit.is_some() && trace_enabled() {
            // Same wording as upstream's trace. t-credentials.sh greps
            // for it.
            let mut e = std::io::stderr().lock();
            let _ = writeln!(
                e,
                "creds: git credential cache ({:?}, {:?}, {:?})",
                query.protocol, query.host, query.path,
            );
        }
        Ok(hit)
    }

    /// Cache `creds` for `query`.
    ///
    /// Returns [`HelperOutcome::Continue`] the first time, so a helper
    /// that stores credentials still sees the call. Later calls for the
    /// same query return [`HelperOutcome::Handled`] without replacing the
    /// cached entry, so `git credential approve` doesn't run per request.
    /// Same as upstream's `credentialCacher.Approve` in `creds/creds.go`.
    fn approve(&self, query: &Query, creds: &Credentials) -> Result<HelperOutcome, HelperError> {
        let mut cache = self.cache.lock().unwrap();
        if cache.contains_key(query) {
            return Ok(HelperOutcome::Handled);
        }
        cache.insert(query.clone(), creds.clone());
        Ok(HelperOutcome::Continue)
    }

    /// Drop the cached entry for `query`.
    ///
    /// Returns [`HelperOutcome::Continue`] so later helpers, such as
    /// `git credential`, also reject the credentials.
    fn reject(&self, query: &Query, _creds: &Credentials) -> Result<HelperOutcome, HelperError> {
        self.cache.lock().unwrap().remove(query);
        Ok(HelperOutcome::Continue)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn q() -> Query {
        Query {
            protocol: "https".into(),
            host: "git.example.com".into(),
            path: String::new(),
        }
    }

    #[test]
    fn fill_misses_until_approve() {
        let h = CachingHelper::new();
        assert!(h.fill(&q()).unwrap().is_none());
        let c = Credentials::new("alice", "hunter2");
        let outcome = h.approve(&q(), &c).unwrap();
        assert_eq!(outcome, HelperOutcome::Continue);
        assert_eq!(h.fill(&q()).unwrap(), Some(c));
    }

    #[test]
    fn second_approve_returns_handled() {
        let h = CachingHelper::new();
        let c = Credentials::new("alice", "hunter2");
        assert_eq!(h.approve(&q(), &c).unwrap(), HelperOutcome::Continue);
        assert_eq!(h.approve(&q(), &c).unwrap(), HelperOutcome::Handled);
    }

    #[test]
    fn reject_evicts() {
        let h = CachingHelper::new();
        let c = Credentials::new("alice", "hunter2");
        h.approve(&q(), &c).unwrap();
        h.reject(&q(), &c).unwrap();
        assert!(h.fill(&q()).unwrap().is_none());
    }

    #[test]
    fn fill_keys_on_full_query_tuple() {
        let h = CachingHelper::new();
        let c = Credentials::new("alice", "hunter2");
        h.approve(&q(), &c).unwrap();
        let other = Query {
            protocol: "https".into(),
            host: "other.example.com".into(),
            path: String::new(),
        };
        assert!(h.fill(&other).unwrap().is_none());
    }
}
