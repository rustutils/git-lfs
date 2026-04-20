//! Process-local credential cache.
//!
//! Avoids re-shelling-out to `git credential` for every request once we've
//! resolved a working set of credentials for a given (protocol, host, path).
//!
//! Scope is a single CLI invocation — for a long-running daemon you'd want
//! a TTL on top of this. Not relevant for our short-lived CLI subcommands.

use std::collections::HashMap;
use std::sync::Mutex;

use crate::helper::{Credentials, Helper, HelperError};
use crate::query::Query;

#[derive(Debug, Default)]
pub struct CachingHelper {
    cache: Mutex<HashMap<Query, Credentials>>,
}

impl CachingHelper {
    pub fn new() -> Self {
        Self::default()
    }
}

impl Helper for CachingHelper {
    fn fill(&self, query: &Query) -> Result<Option<Credentials>, HelperError> {
        Ok(self.cache.lock().unwrap().get(query).cloned())
    }

    /// Cache the working credentials so the next request skips the helper
    /// chain entirely.
    fn approve(&self, query: &Query, creds: &Credentials) -> Result<(), HelperError> {
        self.cache
            .lock()
            .unwrap()
            .insert(query.clone(), creds.clone());
        Ok(())
    }

    /// Drop the cached entry — whatever's in there clearly didn't work.
    fn reject(&self, query: &Query, _creds: &Credentials) -> Result<(), HelperError> {
        self.cache.lock().unwrap().remove(query);
        Ok(())
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
        h.approve(&q(), &c).unwrap();
        assert_eq!(h.fill(&q()).unwrap(), Some(c));
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
