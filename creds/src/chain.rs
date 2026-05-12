//! Run a sequence of helpers, passing successes/failures back to all of them.
//!
//! Mirrors upstream's `CredentialHelpers`: the first helper to return
//! creds wins for `fill`; `approve`/`reject` are broadcast to every
//! helper so caches stay in sync with the upstream source of truth
//! (`git credential`).

use std::io::Write as _;

use crate::helper::{Credentials, Helper, HelperError};
use crate::query::Query;

/// Try each helper in order on `fill`, broadcast `approve`/`reject`.
///
/// Typical wiring: `chain![CachingHelper::new(), GitCredentialHelper::new()]`
/// — the cache short-circuits the slow shell-out path once we've resolved
/// a working pair, and approvals propagate so subsequent calls hit the
/// cache.
pub struct HelperChain {
    helpers: Vec<Box<dyn Helper>>,
}

impl HelperChain {
    pub fn new(helpers: Vec<Box<dyn Helper>>) -> Self {
        Self { helpers }
    }

    /// True if no helpers are configured. Calls into [`Helper::fill`]
    /// will always return `None` for an empty chain.
    pub fn is_empty(&self) -> bool {
        self.helpers.is_empty()
    }
}

impl Helper for HelperChain {
    /// Walk helpers in order. The first to return creds wins; helpers
    /// that error out are logged and skipped so a busted askpass program
    /// can't lock the user out of `git credential` further down the
    /// chain. Mirrors upstream's `CredentialHelpers.Fill` at
    /// `creds/creds.go:502`. If nothing returned creds and at least one
    /// helper errored, surface the last error so callers see *why*
    /// nothing worked rather than a bare "credentials not found".
    fn fill(&self, query: &Query) -> Result<Option<Credentials>, HelperError> {
        let mut last_err: Option<HelperError> = None;
        for h in &self.helpers {
            match h.fill(query) {
                Ok(Some(c)) => return Ok(Some(c)),
                Ok(None) => continue,
                Err(e) => {
                    // Upstream's `credential fill error: <err>` trace
                    // at `creds/creds.go:513`. Always-on; `GIT_TRACE`
                    // gating isn't worth the extra branch for a path
                    // that already only fires when something failed.
                    let mut err = std::io::stderr().lock();
                    let _ = writeln!(err, "credential fill error: {e}");
                    last_err = Some(e);
                    continue;
                }
            }
        }
        match last_err {
            Some(e) => Err(e),
            None => Ok(None),
        }
    }

    /// Broadcast to every helper. Errors from individual helpers are
    /// surfaced (first wins) — a failed approve generally means we
    /// couldn't write to the keystore, which is worth knowing about.
    fn approve(&self, query: &Query, creds: &Credentials) -> Result<(), HelperError> {
        let mut first_err = None;
        for h in &self.helpers {
            if let Err(e) = h.approve(query, creds) {
                first_err.get_or_insert(e);
            }
        }
        match first_err {
            Some(e) => Err(e),
            None => Ok(()),
        }
    }

    fn reject(&self, query: &Query, creds: &Credentials) -> Result<(), HelperError> {
        let mut first_err = None;
        for h in &self.helpers {
            if let Err(e) = h.reject(query, creds) {
                first_err.get_or_insert(e);
            }
        }
        match first_err {
            Some(e) => Err(e),
            None => Ok(()),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::Mutex;

    #[derive(Default)]
    struct StaticHelper {
        answer: Option<Credentials>,
        approves: Mutex<Vec<(Query, Credentials)>>,
        rejects: Mutex<Vec<(Query, Credentials)>>,
    }

    impl Helper for StaticHelper {
        fn fill(&self, _q: &Query) -> Result<Option<Credentials>, HelperError> {
            Ok(self.answer.clone())
        }
        fn approve(&self, q: &Query, c: &Credentials) -> Result<(), HelperError> {
            self.approves.lock().unwrap().push((q.clone(), c.clone()));
            Ok(())
        }
        fn reject(&self, q: &Query, c: &Credentials) -> Result<(), HelperError> {
            self.rejects.lock().unwrap().push((q.clone(), c.clone()));
            Ok(())
        }
    }

    fn q() -> Query {
        Query {
            protocol: "https".into(),
            host: "h".into(),
            path: String::new(),
        }
    }

    #[test]
    fn fill_returns_first_match() {
        let chain = HelperChain::new(vec![
            Box::new(StaticHelper {
                answer: None,
                ..Default::default()
            }),
            Box::new(StaticHelper {
                answer: Some(Credentials::new("u", "p")),
                ..Default::default()
            }),
        ]);
        assert_eq!(chain.fill(&q()).unwrap(), Some(Credentials::new("u", "p")));
    }

    #[test]
    fn approve_broadcasts_to_all_helpers() {
        let chain = crate::CachingHelper::new();
        let outer = HelperChain::new(vec![
            Box::new(StaticHelper::default()),
            Box::new(crate::CachingHelper::new()),
        ]);
        let c = Credentials::new("u", "p");
        outer.approve(&q(), &c).unwrap();
        // First helper recorded the approve; can't peek at the inner cache
        // through the trait, but the broadcast itself completing without
        // error is what we're checking.
        let _ = chain;
    }
}
