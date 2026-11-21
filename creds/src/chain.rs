//! Ask several credential helpers in order.

use std::io::Write as _;

use crate::helper::{Credentials, FillContext, Helper, HelperError, HelperOutcome};
use crate::query::Query;

/// A [`Helper`] that asks a list of helpers in order.
///
/// Fill returns the first credentials found. Approve and reject stop at the
/// first helper that returns [`HelperOutcome::Handled`], like upstream's
/// `CredentialHelpers` in `creds/creds.go`.
///
/// A helper that fails doesn't stop the chain. Fill prints
/// `credential fill error: <err>` to stderr and moves on. If no helper
/// finds credentials, it returns the last error. Approve and reject return
/// the first error, unless a later helper handles the call.
///
/// Put a [`CachingHelper`] before a [`GitCredentialHelper`] so that
/// `git credential` runs only once per query.
///
/// [`CachingHelper`]: crate::CachingHelper
/// [`GitCredentialHelper`]: crate::GitCredentialHelper
pub struct HelperChain {
    helpers: Vec<Box<dyn Helper>>,
}

impl HelperChain {
    /// Build a chain that asks `helpers` in order.
    pub fn new(helpers: Vec<Box<dyn Helper>>) -> Self {
        Self { helpers }
    }

    /// Whether the chain has no helpers.
    ///
    /// Fill on an empty chain always returns `Ok(None)`.
    pub fn is_empty(&self) -> bool {
        self.helpers.is_empty()
    }
}

impl Helper for HelperChain {
    fn fill(&self, query: &Query) -> Result<Option<Credentials>, HelperError> {
        self.fill_with_context(query, &FillContext::default())
    }

    fn fill_with_context(
        &self,
        query: &Query,
        ctx: &FillContext,
    ) -> Result<Option<Credentials>, HelperError> {
        let mut last_err: Option<HelperError> = None;
        for h in &self.helpers {
            match h.fill_with_context(query, ctx) {
                Ok(Some(c)) => return Ok(Some(c)),
                Ok(None) => continue,
                Err(e) => {
                    // Upstream prints this only under `GIT_TRACE`. Here it
                    // always prints, since it only fires on failure.
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
        // The cache returns Continue on the first approve, so a helper
        // after it would run, and Handled on the second, so it wouldn't.
        let cache = crate::CachingHelper::new();
        let chain = HelperChain::new(vec![Box::new(cache)]);
        let c = Credentials::new("u", "p");
        assert_eq!(chain.approve(&q(), &c).unwrap(), HelperOutcome::Continue);
        assert_eq!(chain.approve(&q(), &c).unwrap(), HelperOutcome::Handled);
    }
}
