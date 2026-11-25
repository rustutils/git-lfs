//! The LFS API, and the routes around it: auth checks, redirects, verify,
//! and rate limits.

use std::collections::BTreeMap;
use std::io::Write;
use std::time::{Duration, SystemTime};

use base64::Engine as _;
use serde::{Deserialize, Serialize};

use crate::http::{
    Request, Resp, add_header, canonical_name, error, json_line, lfs_error, query_escape, respond,
    status,
};
use crate::state::{REFILL_TOKEN_COUNT, State, resource_key, rfc3339};
use crate::{git, locks};

const LFS_JSON: &str = "application/vnd.git-lfs+json";

const HEADER_REDIRECT_AUTH_DOWNLOAD_REPO: &str = "header-redirect-download-auth";
const HEADER_REDIRECT_DIRECT_DOWNLOAD_REPO: &str = "header-redirect-download-direct";
const HEADER_REDIRECT_COOKIE_REPO: &str = "header-redirect-cookie";
const HEADER_REDIRECT_SOURCE_MARKER: &str = "lfs-source-marker";
const HEADER_REDIRECT_SOURCE_COOKIE: &str = "lfs-source-cookie";

/// Requests no other route claims: the LFS API under `<repo>/info/lfs`,
/// and Git's smart HTTP protocol.
pub(crate) async fn root(state: &State, req: &Request) -> Resp {
    if req.path.starts_with("/require-cookie-") && !req.header("Cookie").contains("secret") {
        return status(403);
    }

    if !req.path.contains("/info/lfs") {
        return git::http_backend(state, req).await;
    }

    let repo = repo_from_lfs_url(&req.path);
    if let Ok(repo) = &repo
        && req.path.ends_with("/objects/batch")
        && let Some(resp) = record_header_redirect(state, req, repo, "batch-target")
    {
        return resp;
    }

    // The header redirect fixtures must accept requests after the client
    // drops the Authorization header to follow a redirect.
    let skip_auth = repo.as_deref().is_ok_and(is_header_redirect_repo);
    if !skip_auth && let Some(resp) = check_auth(req) {
        return resp;
    }
    let mut resp = lfs(state, req, repo);
    if !resp.headers().contains_key("content-type") {
        add_header(&mut resp, "Content-Type", LFS_JSON);
    }
    resp
}

fn lfs(state: &State, req: &Request, repo: Result<String, String>) -> Resp {
    let repo = match repo {
        Ok(repo) => repo,
        Err(e) => return respond(500, &[], e),
    };
    if !req.header("Accept").contains(LFS_JSON) {
        return status(406);
    }
    match req.method.as_str() {
        "POST" => {
            if !req.header("Content-Type").contains(LFS_JSON) {
                return status(400);
            }
            if req.url_string().ends_with("batch") {
                batch(state, req, &repo)
            } else {
                locks::handle(state, req, &repo)
            }
        }
        "DELETE" => {
            let oid = req.path.rsplit('/').next().unwrap_or("");
            state.objects.lock().unwrap().delete(&repo, oid);
            status(200)
        }
        "GET" if req.url_string().contains("/locks") => locks::handle(state, req, &repo),
        "GET" => respond(404, &[], "lock request"),
        _ => status(405),
    }
}

/// The repository in an LFS URL path such as `/repo.git/info/lfs/...`,
/// without `.git`.
pub(crate) fn repo_from_lfs_url(path: &str) -> Result<String, String> {
    let trimmed = path.strip_prefix('/').unwrap_or(path);
    match trimmed.split_once("/info/lfs") {
        Some((repo, _)) if !repo.is_empty() && !repo.contains('/') => {
            Ok(repo.strip_suffix(".git").unwrap_or(repo).to_owned())
        }
        _ => Err(format!(
            "LFS url '{path}' does not match \\A/?([^/]+)/info/lfs"
        )),
    }
}

fn is_header_redirect_repo(repo: &str) -> bool {
    repo.strip_prefix("header-redirect-").is_some_and(|rest| {
        !rest.is_empty()
            && rest
                .bytes()
                .all(|b| b.is_ascii_lowercase() || b.is_ascii_digit() || b == b'-')
    })
}

/// For the header redirect fixtures, append to `<repo>.headers` the names
/// of the headers that carry the source marker, so tests can check which
/// headers survived a redirect. Returns an error response if the file
/// can't be written.
pub(crate) fn record_header_redirect(
    state: &State,
    req: &Request,
    repo: &str,
    stage: &str,
) -> Option<Resp> {
    if !is_header_redirect_repo(repo) {
        return None;
    }
    let mut names: Vec<String> = Vec::new();
    for name in req.headers.keys() {
        let values = req.headers.get_all(name);
        let carries = if name == "cookie" {
            values.iter().filter_map(|v| v.to_str().ok()).any(|v| {
                v.split(';').any(|c| {
                    c.trim().split_once('=')
                        == Some((HEADER_REDIRECT_SOURCE_COOKIE, HEADER_REDIRECT_SOURCE_MARKER))
                })
            })
        } else {
            values
                .iter()
                .any(|v| v.as_bytes() == HEADER_REDIRECT_SOURCE_MARKER.as_bytes())
        };
        if carries {
            names.push(canonical_name(name.as_str()));
        }
    }
    names.sort();
    let path = state.repo_dir.join(format!("{repo}.headers"));
    std::fs::OpenOptions::new()
        .append(true)
        .create(true)
        .open(&path)
        .and_then(|mut f| writeln!(f, "{stage} {}", names.join(",")))
        .err()
        .map(|e| error(500, &e.to_string()))
}

/// The scheme, user, and credential from an `Authorization` header.
///
/// Basic gives the user and password, and Bearer and Multistage give the
/// token as the credential. Anything else gives empty strings.
pub(crate) fn extract_auth(auth: &str) -> Result<(&'static str, String, String), String> {
    if let Some(encoded) = auth.strip_prefix("Basic ") {
        let decoded = base64::engine::general_purpose::STANDARD
            .decode(encoded)
            .map_err(|e| e.to_string())?;
        let decoded = String::from_utf8_lossy(&decoded).into_owned();
        return Ok(match decoded.split_once(':') {
            Some((user, pass)) => ("Basic", user.to_owned(), pass.to_owned()),
            None => ("", String::new(), String::new()),
        });
    }
    for scheme in ["Bearer", "Multistage"] {
        if let Some(cred) = auth
            .strip_prefix(scheme)
            .and_then(|rest| rest.strip_prefix(' '))
        {
            return Ok((scheme, String::new(), cred.to_owned()));
        }
    }
    Ok(("", String::new(), String::new()))
}

/// Check the credentials for an LFS request. Returns the rejection, if any.
fn check_auth(req: &Request) -> Option<Resp> {
    let (mut wanted, header) = if req.path.starts_with("/auth-bearer") {
        ("Bearer", "Www-Authenticate")
    } else if req.path.starts_with("/auth-multistage") {
        ("Multistage type=foo", "Www-Authenticate")
    } else {
        ("Basic realm=\"testsuite\"", "Lfs-Authenticate")
    };

    let auth = req.header("Authorization");
    if auth.is_empty() {
        return Some(respond(401, &[(header, wanted)], ""));
    }
    let Ok((scheme, user, cred)) = extract_auth(auth) else {
        return Some(status(403));
    };
    if !wanted.starts_with(scheme) {
        return Some(status(403));
    }

    match scheme {
        "Basic" => match user.as_str() {
            "user" if cred == "pass" => return None,
            "netrcuser" | "requirecreds" => return None,
            "path" if req.path.starts_with(&format!("/{cred}")) => return None,
            _ => {}
        },
        "Bearer" if cred == "token" => return None,
        "Multistage" => {
            if let Some((stage, total)) = cred
                .strip_prefix("cred")
                .and_then(|rest| rest.split_once("of"))
                && !stage.is_empty()
                && !total.is_empty()
                && stage.bytes().all(|b| b.is_ascii_digit())
                && total.bytes().all(|b| b.is_ascii_digit())
            {
                if stage == total {
                    return None;
                }
                wanted = "Multistage type=bar";
                return Some(respond(401, &[(header, wanted)], ""));
            }
        }
        _ => {}
    }

    let repo = repo_from_lfs_url(&req.path).unwrap_or_default();
    Some(status(if repo.ends_with("-401-unauth") {
        401
    } else {
        403
    }))
}

/// For `requirecreds*` repositories, reject requests without the
/// `requirecreds:pass` credentials.
pub(crate) fn missing_required_creds(req: &Request, repo: &str) -> Option<Resp> {
    if !repo.starts_with("requirecreds") {
        return None;
    }
    let auth = req.header("Authorization");
    if auth.is_empty() {
        return Some(lfs_error(401, "Error: Authorization Required"));
    }
    match extract_auth(auth) {
        Err(e) => Some(lfs_error(403, &e)),
        Ok((_, user, pass)) if user != "requirecreds" || pass != "pass" => Some(lfs_error(
            403,
            &format!("Got: '{auth}' => '{user}' : '{pass}'"),
        )),
        Ok(_) => None,
    }
}

/// For `*-<branch>-branch-required` repositories, reject a request whose
/// ref isn't `refs/heads/<branch>`.
pub(crate) fn check_required_branch(repo: &str, got: &str) -> Option<Resp> {
    if !repo.ends_with("branch-required") {
        return None;
    }
    let parts: Vec<&str> = repo.split('-').collect();
    if parts.len() <= 3 {
        return None;
    }
    let want = format!("refs/heads/{}", parts[parts.len() - 3]);
    if want == got {
        return None;
    }
    let message = format!("Expected ref {want:?}, got {got:?}");
    Some(respond(
        403,
        &[],
        json_line(&serde_json::json!({ "message": message })),
    ))
}

#[derive(Deserialize)]
pub(crate) struct RefName {
    #[serde(default)]
    pub name: Option<String>,
}

/// The `name` of an optional `ref` object, or `""`.
pub(crate) fn ref_name(r: &Option<RefName>) -> &str {
    r.as_ref().and_then(|r| r.name.as_deref()).unwrap_or("")
}

#[derive(Deserialize)]
struct BatchRequest {
    #[serde(default)]
    transfers: Option<Vec<String>>,
    #[serde(default)]
    operation: Option<String>,
    #[serde(default)]
    objects: Option<Vec<RequestObject>>,
    #[serde(default, rename = "ref")]
    r#ref: Option<RefName>,
}

#[derive(Deserialize)]
struct RequestObject {
    #[serde(default)]
    oid: Option<String>,
    #[serde(default)]
    size: Option<i64>,
}

/// A batch response, with the fields Go's encoding omits when empty.
#[derive(Serialize)]
struct BatchResponse {
    #[serde(skip_serializing_if = "String::is_empty")]
    transfer: String,
    objects: Vec<Object>,
    #[serde(skip_serializing_if = "String::is_empty")]
    hash_algo: String,
}

#[derive(Serialize)]
struct Object {
    #[serde(skip_serializing_if = "String::is_empty")]
    oid: String,
    #[serde(skip_serializing_if = "is_zero")]
    size: i64,
    #[serde(skip_serializing_if = "is_false")]
    authenticated: bool,
    #[serde(skip_serializing_if = "BTreeMap::is_empty")]
    actions: BTreeMap<String, Link>,
    #[serde(rename = "_links", skip_serializing_if = "BTreeMap::is_empty")]
    links: BTreeMap<String, Link>,
    #[serde(skip_serializing_if = "Option::is_none")]
    error: Option<ObjectError>,
}

/// An action. Go always encodes `expires_at`, as the zero time when
/// unset, so this does too.
#[derive(Serialize)]
struct Link {
    href: String,
    #[serde(skip_serializing_if = "BTreeMap::is_empty")]
    header: BTreeMap<String, String>,
    expires_at: String,
    #[serde(skip_serializing_if = "is_zero")]
    expires_in: i64,
}

#[derive(Serialize)]
struct ObjectError {
    #[serde(skip_serializing_if = "is_zero")]
    code: i64,
    message: String,
}

const ZERO_TIME: &str = "0001-01-01T00:00:00Z";

fn is_zero(n: &i64) -> bool {
    *n == 0
}

fn is_false(b: &bool) -> bool {
    !*b
}

fn batch(state: &State, req: &Request, repo: &str) -> Resp {
    let mut cookie = None;
    if repo == HEADER_REDIRECT_COOKIE_REPO {
        cookie = Some(format!(
            "{HEADER_REDIRECT_SOURCE_COOKIE}={HEADER_REDIRECT_SOURCE_MARKER}; Path=/"
        ));
    }
    let with_cookie = |mut resp: Resp| {
        if let Some(c) = &cookie {
            add_header(&mut resp, "Set-Cookie", c);
        }
        resp
    };

    let checking_object = req.header("X-Check-Object") == "1";
    if !checking_object && repo == "batchunsupported" {
        return with_cookie(status(404));
    }
    if !checking_object && repo == "badbatch" {
        return with_cookie(status(203));
    }
    if repo == "netrctest" {
        let ok = matches!(
            extract_auth(req.header("Authorization")),
            Ok((_, user, pass)) if user == "netrcuser" && pass == "netrcpass"
        );
        if !ok {
            return with_cookie(status(403));
        }
    }
    if let Some(resp) = missing_required_creds(req, repo) {
        return with_cookie(resp);
    }

    let request: BatchRequest = match serde_json::from_slice(&req.body) {
        Ok(r) => r,
        Err(e) => return with_cookie(respond(400, &[], e.to_string())),
    };

    if let Some(resp) = check_required_branch(repo, ref_name(&request.r#ref)) {
        return with_cookie(resp);
    }

    let retry_later =
        repo.ends_with("batch-retry-later") || repo.ends_with("batch-retry-later-exceeds-max");
    if retry_later && let Some(wait) = state.check_rate_limit(&resource_key("batch", "", repo, ""))
    {
        let wait = wait.to_string();
        return with_cookie(respond(
            429,
            &[("Retry-After", wait.as_str())],
            "rate limit reached",
        ));
    }
    if repo.ends_with("batch-retry-later-no-header")
        && state
            .check_rate_limit(&resource_key("batch", "", repo, ""))
            .is_some()
    {
        return with_cookie(respond(429, &[], "rate limit reached"));
    }

    let search_for = if repo.starts_with("batch-storage-upload-tus") {
        "tus"
    } else if repo.starts_with("custom-transfer-") {
        "testcustom"
    } else {
        ""
    };
    let transfer =
        if !search_for.is_empty() && request.transfers.iter().flatten().any(|t| t == search_for) {
            search_for.to_owned()
        } else {
            String::new()
        };
    let chunked = repo == "batch-storage-upload-encoding-chunked";
    let tus_interrupt = repo == "batch-storage-upload-tus-interrupt";
    let operation = request.operation.unwrap_or_default();

    let mut hash_algo = "sha256".to_owned();
    let mut objects = Vec::new();
    for obj in request.objects.unwrap_or_default() {
        let oid = obj.oid.unwrap_or_default();
        let handler = state.handler(&oid);
        let exists = state.objects.lock().unwrap().get(repo, &oid).is_some();
        let mut o = Object {
            oid: if handler == "unknown-oid" {
                "unknown-oid".to_owned()
            } else {
                oid.clone()
            },
            size: obj.size.unwrap_or(0),
            authenticated: false,
            actions: BTreeMap::new(),
            links: BTreeMap::new(),
            error: None,
        };

        let mut add_action = true;
        if operation == "download" {
            if !exists {
                o.error = Some(ObjectError {
                    code: 404,
                    message: format!("Object {oid} does not exist"),
                });
                add_action = false;
            }
        } else if exists {
            // Not an error, but there's nothing to upload.
            add_action = false;
        }

        if handler == "object-authenticated"
            || (repo == HEADER_REDIRECT_DIRECT_DOWNLOAD_REPO && operation == "download")
        {
            o.authenticated = true;
        }

        let batch_status = handler
            .strip_prefix("status-batch-")
            .and_then(|code| code.parse::<i64>().ok());
        if let Some(code) = batch_status {
            o.error = Some(ObjectError {
                code,
                message: "welp".to_owned(),
            });
        } else {
            if handler == "return-invalid-size" {
                o.size = -1;
            }
            if handler == "batch-hash-algo-empty" {
                hash_algo = String::new();
            } else if handler == "batch-hash-algo-invalid" {
                hash_algo = "invalid".to_owned();
            }
            if add_action {
                let mut link = Link {
                    href: lfs_url(state, repo, &oid, handler == "redirect-storage-upload"),
                    header: BTreeMap::new(),
                    expires_at: ZERO_TIME.to_owned(),
                    expires_in: 0,
                };
                serve_expired(state, &mut link, repo, handler);
                if chunked {
                    link.header
                        .insert("Transfer-Encoding".into(), "chunked".into());
                }
                if tus_interrupt {
                    link.header
                        .insert("Lfs-Tus-Interrupt".into(), "true".into());
                }
                if handler == "send-deprecated-links" {
                    o.links.insert(operation.clone(), link);
                } else {
                    o.actions.insert(operation.clone(), link);
                }
            }
            if handler == "send-verify-action" {
                o.actions.insert(
                    "verify".into(),
                    Link {
                        href: format!("{}/verify", state.url),
                        header: BTreeMap::from([("repo".to_owned(), repo.to_owned())]),
                        expires_at: ZERO_TIME.to_owned(),
                        expires_in: 0,
                    },
                );
            }
        }
        objects.push(o);
    }

    let body = serde_json::to_string(&BatchResponse {
        transfer,
        objects,
        hash_algo,
    })
    .expect("serializable");
    with_cookie(respond(200, &[], body))
}

/// The URL of an object in storage, or of a redirect to it.
fn lfs_url(state: &State, repo: &str, oid: &str, redirect: bool) -> String {
    let repo_param = query_escape(repo);
    if redirect {
        let mut host = state.url.clone();
        if matches!(
            repo,
            HEADER_REDIRECT_AUTH_DOWNLOAD_REPO
                | HEADER_REDIRECT_DIRECT_DOWNLOAD_REPO
                | HEADER_REDIRECT_COOKIE_REPO
        ) {
            // Start on localhost, so the redirect to 127.0.0.1 changes host.
            host = host.replacen("127.0.0.1", "localhost", 1);
        }
        return format!("{host}/redirect307/objects/{oid}?r={repo_param}");
    }
    format!("{}/storage/{oid}?r={repo_param}", state.url)
}

/// Make the action expired for the fixtures that want one.
fn serve_expired(state: &State, link: &mut Link, repo: &str, handler: &str) {
    let at = rfc3339(SystemTime::now() - Duration::from_secs(5 * 60));
    let wants_expired = match handler {
        "return-expired-action-forever" => state.serve_expired(repo, true),
        "return-expired-action" => state.serve_expired(repo, false),
        _ => false,
    };
    if wants_expired {
        link.expires_at = at;
        return;
    }
    match repo {
        "expired-absolute" => link.expires_at = at,
        "expired-relative" => link.expires_in = -5,
        "expired-both" => {
            link.expires_at = at;
            link.expires_in = -5;
        }
        _ => {}
    }
}

/// `POST /verify`: succeed, or for `*verify-fail-<n>-times` repositories,
/// fail until the `n`th attempt.
pub(crate) fn verify(state: &State, req: &Request) -> Resp {
    #[derive(Deserialize)]
    struct Payload {
        #[serde(default)]
        oid: String,
    }
    let repo = req.header("repo");
    let payload: Payload = match serde_json::from_slice(&req.body) {
        Ok(p) => p,
        Err(e) => return lfs_error(422, &e.to_string()),
    };
    let Some(max) = verify_fail_count(repo) else {
        return status(200);
    };
    let count = state.count_verify(repo, &payload.oid);
    if count < max {
        return lfs_error(
            503,
            &format!("intentionally failing verify request {count} (out of {max})"),
        );
    }
    status(200)
}

/// `n` from a repository name ending in `verify-fail-<n>-time` or
/// `verify-fail-<n>-times`.
fn verify_fail_count(repo: &str) -> Option<u32> {
    let rest = repo.strip_suffix('s').unwrap_or(repo);
    let rest = rest.strip_suffix("-time")?;
    let (_, n) = rest.rsplit_once("verify-fail-")?;
    if n.is_empty() || !n.bytes().all(|b| b.is_ascii_digit()) {
        return None;
    }
    n.parse().ok()
}

/// `/redirect307/<route>/<path>`: a 307 to `<path>` on the same server
/// (`rel`), the main server (`abs`), HTTPS on the same host (`ssl`), or in
/// storage (`objects`).
pub(crate) fn redirect307(state: &State, req: &Request) -> Resp {
    let parts: Vec<&str> = req.path.split('/').collect();
    if parts.len() < 3 {
        return crate::http::not_found();
    }
    let mut repo = parts
        .get(3)
        .map(|p| p.strip_suffix(".git").unwrap_or(p).to_owned())
        .unwrap_or_default();
    let rest = parts.get(3..).unwrap_or(&[]).join("/");
    let mut stage = "batch-source";
    let location = match parts[2] {
        "rel" => format!("/{rest}"),
        "abs" => format!("{}/{rest}", state.url),
        "ssl" => format!("https://{}/{rest}", req.header("Host")),
        "objects" => {
            repo = req.query_param("r");
            stage = "object-source";
            format!("{}/storage/{rest}?r={repo}", state.url)
        }
        _ => return status(404),
    };
    if let Some(resp) = record_header_redirect(state, req, &repo, stage) {
        return resp;
    }
    respond(307, &[("Location", location.as_str())], "")
}

/// `/limits/`: set a resource's rate limit budget.
pub(crate) fn limits(state: &State, req: &Request) -> Resp {
    let tokens = req.query_param("tokens");
    let tokens = tokens.parse::<i64>().unwrap_or(if tokens == "max" {
        REFILL_TOKEN_COUNT
    } else {
        0
    });
    let key = resource_key(
        &req.query_param("api"),
        &req.query_param("direction"),
        &req.query_param("repo"),
        &req.query_param("oid"),
    );
    state.set_rate_limit(&key, tokens);
    status(200)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn repo_comes_from_the_lfs_url() {
        assert_eq!(
            repo_from_lfs_url("/a.git/info/lfs/objects/batch").unwrap(),
            "a"
        );
        assert_eq!(repo_from_lfs_url("b/info/lfs/locks").unwrap(), "b");
        assert!(repo_from_lfs_url("/a/b/info/lfs").is_err());
    }

    #[test]
    fn verify_fail_count_reads_the_repo_name() {
        assert_eq!(verify_fail_count("x-verify-fail-2-times"), Some(2));
        assert_eq!(verify_fail_count("verify-fail-1-time"), Some(1));
        assert_eq!(verify_fail_count("verify-fail-x-times"), None);
        assert_eq!(verify_fail_count("plain"), None);
    }

    #[test]
    fn header_redirect_repos_match() {
        assert!(is_header_redirect_repo("header-redirect-cookie"));
        assert!(!is_header_redirect_repo("header-redirect-"));
        assert!(!is_header_redirect_repo("header-redirect-Cookie"));
    }
}
