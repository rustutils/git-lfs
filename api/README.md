# git-lfs-api

Async HTTP client for the [Git LFS batch and locking APIs](https://github.com/git-lfs/git-lfs/blob/main/docs/api/README.md),
built on `reqwest` (rustls).

Speaks both halves of the LFS server protocol:

- **Batch** — `POST /objects/batch` to negotiate transfers, then
  follow the returned upload/download/verify actions.
- **Locking** — list, create, verify, and delete file locks
  (`/locks`, `/locks/verify`, `/locks/{id}/unlock`).

The client routes every request through a 401 → credential-fill →
retry-once → approve/reject loop, with an in-memory cache so
subsequent requests skip the helper. Credential resolution is
delegated to [`git-lfs-creds`](https://crates.io/crates/git-lfs-creds).

Part of the [git-lfs Rust workspace](https://gitlab.com/rustutils/git-lfs).
Experimental — not yet ready for production. License: MIT.
