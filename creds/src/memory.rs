//! Process-local credential cache.
//!
//! Avoids re-shelling-out to `git credential` for every request once we've
//! resolved a working set of credentials for a given (protocol, host, path).
//!
//! Scope is a single CLI invocation — for a long-running daemon you'd want
//! a TTL on top of this. Not relevant for our short-lived CLI subcommands.

use std::collections::HashMap;
use std::io::Write as _;
use std::sync::Mutex;

use crate::helper::{Credentials, Helper, HelperError, HelperOutcome};
use crate::query::Query;
use crate::trace::trace_enabled;

/// Process-local credential cache, keyed on the full [`Query`] tuple.
///
/// Populated by [`Helper::approve`] and consulted on [`Helper::fill`];
/// [`Helper::reject`] drops the corresponding entry. Lives for one
/// CLI invocation; a long-running daemon would want a TTL layered on
/// top.
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
            // Mirrors upstream's `creds: git credential cache (%q, %q, %q)`
            // trace at `creds/creds.go:435`. t-credentials's "bad netrc
            // creds will retry" greps this to confirm the second request
            // reused the just-cached askpass result.
            let mut e = std::io::stderr().lock();
            let _ = writeln!(
                e,
                "creds: git credential cache ({:?}, {:?}, {:?})",
                query.protocol, query.host, query.path,
            );
        }
        Ok(hit)
    }

    /// Cache the working credentials so the next request skips the helper
    /// chain entirely. Returns [`HelperOutcome::Continue`] on the first
    /// approve for a query (so a persisting helper downstream still
    /// gets a turn) and [`HelperOutcome::Handled`] on subsequent calls,
    /// short-circuiting duplicate `git credential approve` invocations
    /// across requests in the same push. Mirrors upstream's
    /// `credentialCacher.Approve` at `creds/creds.go:445`.
    fn approve(&self, query: &Query, creds: &Credentials) -> Result<HelperOutcome, HelperError> {
        let mut cache = self.cache.lock().unwrap();
        if cache.contains_key(query) {
            return Ok(HelperOutcome::Handled);
        }
        cache.insert(query.clone(), creds.clone());
        Ok(HelperOutcome::Continue)
    }

    /// Drop the cached entry — whatever's in there clearly didn't work.
    /// Returns [`HelperOutcome::Continue`] so the chain reaches the
    /// real persistor (typically `git credential reject`).
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
