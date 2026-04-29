# git-lfs-creds

Credential helper bridge for Git LFS — wraps `git credential
fill / approve / reject` so any git-credential helper the user has
configured (osxkeychain, libsecret, manager, store, plain `cache`,
…) Just Works for LFS endpoints too.

Provides:

- A simple `Credentials { username, password }` value type.
- An async `CredentialHelper` that resolves credentials for a URL
  via `git credential fill`, with an in-memory cache so subsequent
  requests skip the round-trip.
- `approve` / `reject` to ratify or evict a cached credential after
  the server's response — same lifecycle git itself uses.

Used by [`git-lfs-api`](https://crates.io/crates/git-lfs-api) for the
401 → fill → retry loop. Not specific to LFS; could be reused for
any HTTPS client that wants to pick up the user's git-credential
configuration.

Part of the [git-lfs Rust workspace](https://gitlab.com/rustutils/git-lfs).
Experimental — not yet ready for production. License: MIT.
