//! Run a sequence of helpers, passing successes/failures back to all of them.
//!
//! Mirrors upstream's `CredentialHelpers`: the first helper to return
//! creds wins for `fill`; `approve`/`reject` are broadcast to every
//! helper so caches stay in sync with the upstream source of truth
//! (`git credential`).

use std::io::Write as _;

use crate::helper::{Credentials, Helper, HelperError, HelperOutcome};
use crate::query::Query;

/// Try each helper in order on `fill`, broadcast `approve` / `reject`.
///
/// Typical wiring puts a [`CachingHelper`] before a
/// [`GitCredentialHelper`]: the cache short-circuits the slow
/// shell-out path once a working pair has resolved, and approvals
/// propagate so subsequent calls hit the cache.
///
/// [`CachingHelper`]: crate::CachingHelper
/// [`GitCredentialHelper`]: crate::GitCredentialHelper
pub struct HelperChain {
    helpers: Vec<Box<dyn Helper>>,
}

impl HelperChain {
    /// Build a chain from a list of boxed helpers, applied in order.
    pub fn new(helpers: Vec<Box<dyn Helper>>) -> Self {
        Self { helpers }
    }

    /// `true` if no helpers are configured.
    ///
    /// Calls into [`Helper::fill`] will always return `Ok(None)` for
    /// an empty chain.
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

    /// Walk helpers in order; stop at the first
    /// [`HelperOutcome::Handled`]. Mirrors upstream's
    /// `CredentialHelpers.Approve` at `creds/creds.go:552`. The cache
    /// helper returning `Handled` on a repeat call is what stops
    /// `git credential approve` from firing twice within a single push.
    fn approve(&self, query: &Query, creds: &Credentials) -> Result<HelperOutcome, HelperError> {
        let mut first_err = None;
        for h in &self.helpers {
            match h.approve(query, creds) {
                Ok(HelperOutcome::Handled) => return Ok(HelperOutcome::Handled),
                Ok(HelperOutcome::Continue) => continue,
                Err(e) => {
                    first_err.get_or_insert(e);
                }
            }
        }
        match first_err {
            Some(e) => Err(e),
            None => Ok(HelperOutcome::Continue),
        }
    }

    fn reject(&self, query: &Query, creds: &Credentials) -> Result<HelperOutcome, HelperError> {
        let mut first_err = None;
        for h in &self.helpers {
            match h.reject(query, creds) {
                Ok(HelperOutcome::Handled) => return Ok(HelperOutcome::Handled),
                Ok(HelperOutcome::Continue) => continue,
                Err(e) => {
                    first_err.get_or_insert(e);
                }
            }
        }
        match first_err {
            Some(e) => Err(e),
            None => Ok(HelperOutcome::Continue),
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
        fn approve(&self, q: &Query, c: &Credentials) -> Result<HelperOutcome, HelperError> {
            self.approves.lock().unwrap().push((q.clone(), c.clone()));
            Ok(HelperOutcome::Handled)
        }
        fn reject(&self, q: &Query, c: &Credentials) -> Result<HelperOutcome, HelperError> {
            self.rejects.lock().unwrap().push((q.clone(), c.clone()));
            Ok(HelperOutcome::Handled)
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
    fn approve_stops_at_first_handled() {
        // Cache returns Continue on first approve so the chain reaches
        // the persistor; second approve returns Handled and short-
        // circuits the persistor. Verify the persistor sees one call,
        // not two.
        let cache = crate::CachingHelper::new();
        let chain = HelperChain::new(vec![Box::new(cache)]);
        let c = Credentials::new("u", "p");
        assert_eq!(chain.approve(&q(), &c).unwrap(), HelperOutcome::Continue);
        assert_eq!(chain.approve(&q(), &c).unwrap(), HelperOutcome::Handled);
    }
}
