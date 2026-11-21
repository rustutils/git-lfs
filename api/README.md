# git-lfs-api

HTTP client for the Git LFS batch and locking APIs.

A Git LFS client talks to its server through two APIs. The batch API takes a
list of objects and returns a URL for uploading or downloading each one.  The
locking API lets users lock files, so that two people don't edit the same file
at once. This crate is an async client for both, built on reqwest with rustls.

The transfers themselves happen in [git-lfs-transfer], and credentials come
from [git-lfs-creds].

When the server rejects a request, the client asks its credential helpers for
credentials, and retries up to three times. It supports Basic and Bearer
authentication, other schemes returned by credential helpers, and multistage
handshakes. Once a request succeeds, its credentials are reused for later
requests. For SSH remotes, a resolver can supply the HTTPS URL and headers for
each request (see `SshResolver`).

When a server sends Retry-After with a rate limit or server error, the returned
error includes it, so callers can wait as long as the server asks.

[git-lfs-transfer]: https://crates.io/crates/git-lfs-transfer [git-lfs-creds]:
https://crates.io/crates/git-lfs-creds
