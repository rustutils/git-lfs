# git-lfs-api

HTTP client for the Git LFS batch and locking APIs.

Git LFS speaks to a server over HTTPS using two endpoints. The
batch endpoint (`POST /objects/batch`) takes a list of OIDs and
sizes and returns one transfer URL per object (plus any auth
headers and an expiry window); the locking endpoint suite
(`/locks`, `/locks/verify`, `/locks/{id}/unlock`, …) lets clients
claim files to coordinate edits across users on a shared remote.

This crate is the async HTTP client for both. Built on `reqwest`
with rustls, it handles JSON request/response, server-typed
errors, and the auth lifecycle; the actual byte transfer against
the returned URLs lives in [git-lfs-transfer], and credential
resolution lives in [git-lfs-creds].

The client uses two complementary auth mechanisms. An initial
`Auth` (None, Basic, or Bearer) is applied to every request, and
on a 401 the client queries an attached credential helper,
retries once with the filled-in credentials, and reports
`approve` or `reject` to the helper based on the outcome. Once a
fill succeeds, the resolved credentials are cached so subsequent
requests skip the 401 dance. SSH-mediated endpoints
(`git-lfs-authenticate`) hook in via an `SshResolver` trait,
letting a single `Client` transparently swap in a fresh HTTPS
URL and auth headers per request.

Server-supplied `Retry-After` (on 429 or 5xx responses) is
parsed into the typed error so callers can honor the rate-limit
window instead of falling back to exponential backoff. The
`parse_retry_after` helper is exported for reuse on other
response paths.

[git-lfs-transfer]: https://crates.io/crates/git-lfs-transfer
[git-lfs-creds]: https://crates.io/crates/git-lfs-creds
