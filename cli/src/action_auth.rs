//! Credentials for storage requests: [`ActionAuth`] for the transfer queue.
//!
//! Follows upstream's `DoWithAuth` and `getCredURLForAPI`. Whether to send
//! credentials up front comes from `lfs.<url>.access` for the action URL
//! up to the OID. After a 401, `basic` is saved there, so later runs send
//! them at once.
//!
//! The credentials come from, in order:
//!
//! 1. The LFS endpoint URL, when the request goes to its host and the URL
//!    has a password.
//! 2. The Git remote URL, likewise.
//! 3. The credential helpers, asked about the remote URL when it's on the
//!    request's host, else the endpoint URL when that is, else the request
//!    URL.

use std::collections::HashMap;
use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex};

use git_lfs_api::Auth;
use git_lfs_creds::{Credentials, Helper, Query};
use git_lfs_git::ConfigScope;
use git_lfs_transfer::ActionAuth;
use url::Url;

pub struct StorageAuth {
    cwd: PathBuf,
    /// The LFS endpoint, with any credentials in it.
    endpoint: Url,
    /// The Git remote's URL, with any credentials in it.
    remote: Option<Url>,
    helper: Arc<dyn Helper>,
    use_http_path: bool,
    /// Whether each access URL is `basic`, as read or set.
    basic: Mutex<HashMap<String, bool>>,
    /// The helper credentials last given for each access URL.
    filled: Mutex<HashMap<String, (Query, Credentials)>>,
}

impl StorageAuth {
    pub fn new(
        cwd: &Path,
        endpoint: Url,
        remote: Option<Url>,
        helper: Arc<dyn Helper>,
        use_http_path: bool,
    ) -> Self {
        Self {
            cwd: cwd.to_path_buf(),
            endpoint,
            remote,
            helper,
            use_http_path,
            basic: Mutex::new(HashMap::new()),
            filled: Mutex::new(HashMap::new()),
        }
    }

    fn is_basic(&self, access_url: &str) -> bool {
        let mut cache = self.basic.lock().unwrap();
        *cache.entry(access_url.to_owned()).or_insert_with(|| {
            git_lfs_git::lfs_url_value(&self.cwd, access_url, "access")
                .is_some_and(|v| v.trim().eq_ignore_ascii_case("basic"))
        })
    }

    /// The `Authorization` header for a request to `request_url`.
    fn credentials(&self, access_url: &str, request_url: &str) -> Option<String> {
        let request = Url::parse(request_url).ok()?;
        let cred_url = if same_origin(&request, &self.endpoint) {
            if let Some(header) = url_auth(&self.endpoint) {
                return Some(header);
            }
            match self.remote.as_ref().filter(|r| same_origin(r, &request)) {
                Some(remote) => {
                    if let Some(header) = url_auth(remote) {
                        return Some(header);
                    }
                    remote.clone()
                }
                None => self.endpoint.clone(),
            }
        } else {
            request
        };
        let mut query = Query::from_url(&cred_url);
        if !self.use_http_path {
            query = query.without_path();
        }
        let creds = self.helper.fill(&query).ok().flatten()?;
        let header = auth_from(&creds).header_value();
        self.filled
            .lock()
            .unwrap()
            .insert(access_url.to_owned(), (query, creds));
        header
    }
}

impl ActionAuth for StorageAuth {
    fn up_front(&self, access_url: &str, request_url: &str) -> Option<String> {
        if !self.is_basic(access_url) {
            return None;
        }
        self.credentials(access_url, request_url)
    }

    fn unauthorized(&self, access_url: &str, request_url: &str) -> Option<String> {
        if !self.is_basic(access_url) {
            trace("setting repository access to basic");
            let key = format!("lfs.{access_url}.access");
            let _ = git_lfs_git::config::set(&self.cwd, ConfigScope::Local, &key, "basic");
            self.basic
                .lock()
                .unwrap()
                .insert(access_url.to_owned(), true);
        }
        self.credentials(access_url, request_url)
    }

    fn outcome(&self, access_url: &str, worked: bool) {
        let Some((query, creds)) = self.filled.lock().unwrap().remove(access_url) else {
            return;
        };
        let _ = if worked {
            self.helper.approve(&query, &creds)
        } else {
            self.helper.reject(&query, &creds)
        };
    }
}

/// Whether two URLs have the same scheme, host, and port.
fn same_origin(a: &Url, b: &Url) -> bool {
    a.scheme() == b.scheme() && a.host_str() == b.host_str() && a.port() == b.port()
}

/// Basic auth from the user and password in `url`, if it has a password,
/// like upstream's `setRequestAuthFromURL`.
fn url_auth(url: &Url) -> Option<String> {
    let password = url.password()?;
    Auth::Basic {
        username: url.username().to_owned(),
        password: password.to_owned(),
    }
    .header_value()
}

fn auth_from(creds: &Credentials) -> Auth {
    match (&creds.authtype, &creds.credential) {
        (Some(authtype), Some(credential)) => Auth::Custom {
            authtype: authtype.clone(),
            credential: credential.clone(),
        },
        _ => Auth::Basic {
            username: creds.username.clone(),
            password: creds.password.clone(),
        },
    }
}

fn trace(message: &str) {
    if std::env::var_os("GIT_TRACE").is_some_and(|v| !v.is_empty() && v != "0") {
        eprintln!("{message}");
    }
}
