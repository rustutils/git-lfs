//! Credentials for action URLs, like upstream's `DoWithAuth` for storage
//! requests.
//!
//! A request goes without credentials unless the access mode of its URL is
//! `basic`. A 401 to a request without credentials records `basic` and
//! sends the request once more with them. Credentials that work are
//! approved, and ones that get a 401 are rejected.
//!
//! The access mode belongs to the action URL up to the object's OID, such
//! as `https://host/storage/` for `https://host/storage/<oid>?token=x`.

use std::sync::Arc;

/// Supplies and records credentials for action URLs.
///
/// Each method gets the URL the access mode is for, and the full request
/// URL. They may run credential helpers, so they're called on a blocking
/// thread.
pub trait ActionAuth: Send + Sync {
    /// The `Authorization` header to send up front, if the access mode of
    /// `access_url` is `basic`.
    fn up_front(&self, access_url: &str, request_url: &str) -> Option<String>;

    /// A request without credentials got a 401. Record `basic` access for
    /// `access_url` and return the header to try again with.
    fn unauthorized(&self, access_url: &str, request_url: &str) -> Option<String>;

    /// The credentials last returned for `access_url` worked, or got a 401.
    fn outcome(&self, access_url: &str, worked: bool);
}

/// The credential state of one request, across its attempts.
pub(crate) struct RequestAuth {
    auth: Option<Arc<dyn ActionAuth>>,
    access_url: String,
    request_url: String,
    header: Option<String>,
    /// Whether `header` came from `auth`, so its outcome is reported.
    ours: bool,
}

impl RequestAuth {
    /// Start a request to `request_url` for object `oid`.
    ///
    /// `auth` is `None`, so nothing is added, when the request carries
    /// its own credentials.
    pub(crate) async fn start(
        auth: Option<&Arc<dyn ActionAuth>>,
        request_url: &str,
        oid: &str,
    ) -> Self {
        let access_url = request_url
            .split_once(oid)
            .map_or(request_url, |(prefix, _)| prefix)
            .to_owned();
        let mut state = Self {
            auth: auth.cloned(),
            access_url,
            request_url: request_url.to_owned(),
            header: None,
            ours: false,
        };
        if let Some(auth) = state.auth.clone() {
            let (access, request) = (state.access_url.clone(), state.request_url.clone());
            state.header = blocking(move || auth.up_front(&access, &request)).await;
            state.ours = state.header.is_some();
        }
        state
    }

    /// The `Authorization` header to send, if any.
    pub(crate) fn header(&self) -> Option<&str> {
        self.header.as_deref()
    }

    /// Note the response status. Returns true when the request should be
    /// sent again, with the new [`header`](Self::header).
    pub(crate) async fn retry_after(&mut self, status: u16) -> bool {
        let Some(auth) = self.auth.clone() else {
            return false;
        };
        let (access, request) = (self.access_url.clone(), self.request_url.clone());
        match (status, self.header.is_some()) {
            (401, false) => {
                self.header = blocking(move || auth.unauthorized(&access, &request)).await;
                self.ours = self.header.is_some();
                self.ours
            }
            (401, true) if self.ours => {
                blocking(move || auth.outcome(&access, false)).await;
                false
            }
            (200..=299, true) if self.ours => {
                blocking(move || auth.outcome(&access, true)).await;
                false
            }
            _ => false,
        }
    }
}

async fn blocking<T: Send + 'static>(f: impl FnOnce() -> T + Send + 'static) -> T {
    tokio::task::spawn_blocking(f)
        .await
        .expect("credential lookup panicked")
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::Mutex;

    /// Records calls, and is `basic` once `unauthorized` was called.
    #[derive(Default)]
    struct Fake {
        basic: Mutex<bool>,
        calls: Mutex<Vec<String>>,
    }

    impl ActionAuth for Fake {
        fn up_front(&self, access_url: &str, _: &str) -> Option<String> {
            self.calls
                .lock()
                .unwrap()
                .push(format!("up_front {access_url}"));
            self.basic.lock().unwrap().then(|| "Basic x".to_owned())
        }
        fn unauthorized(&self, access_url: &str, _: &str) -> Option<String> {
            self.calls
                .lock()
                .unwrap()
                .push(format!("unauthorized {access_url}"));
            *self.basic.lock().unwrap() = true;
            Some("Basic x".to_owned())
        }
        fn outcome(&self, _: &str, worked: bool) {
            self.calls.lock().unwrap().push(format!("outcome {worked}"));
        }
    }

    const URL: &str = "https://host/storage/abc123?token=1";

    #[tokio::test]
    async fn a_401_without_credentials_retries_once_with_them() {
        let fake = Arc::new(Fake::default());
        let auth: Arc<dyn ActionAuth> = fake.clone();
        let mut state = RequestAuth::start(Some(&auth), URL, "abc123").await;
        assert_eq!(state.header(), None);
        assert!(state.retry_after(401).await);
        assert_eq!(state.header(), Some("Basic x"));
        assert!(!state.retry_after(200).await);
        assert_eq!(
            *fake.calls.lock().unwrap(),
            [
                "up_front https://host/storage/",
                "unauthorized https://host/storage/",
                "outcome true",
            ]
        );
    }

    #[tokio::test]
    async fn rejected_credentials_are_not_retried() {
        let fake = Arc::new(Fake::default());
        *fake.basic.lock().unwrap() = true;
        let auth: Arc<dyn ActionAuth> = fake.clone();
        let mut state = RequestAuth::start(Some(&auth), URL, "abc123").await;
        assert_eq!(state.header(), Some("Basic x"));
        assert!(!state.retry_after(401).await);
        assert_eq!(fake.calls.lock().unwrap().last().unwrap(), "outcome false");
    }

    #[tokio::test]
    async fn without_auth_nothing_happens() {
        let mut state = RequestAuth::start(None, URL, "abc123").await;
        assert_eq!(state.header(), None);
        assert!(!state.retry_after(401).await);
    }
}
