# git-lfs-transfer

Concurrent transfer queue and "basic" adapter for Git LFS object
uploads and downloads.

Sits between [`git-lfs-api`](https://crates.io/crates/git-lfs-api) and
[`git-lfs-store`](https://crates.io/crates/git-lfs-store): given a list
of `(oid, size)` pairs, it negotiates a batch with the LFS server,
spawns a bounded pool of tasks to drive the resulting actions, and
streams progress events back to the caller.

What's implemented today:

- The **basic** transfer adapter (HTTPS upload via PUT, download via
  GET, optional verify callback).
- Concurrent dispatch with per-object error reporting; each transfer
  is independent so partial failures don't tear down the queue.
- Streaming uploads/downloads (no full buffering), and a hash check
  on the download path.
- Retry on transient failures (5xx, 429, network blips) at both the
  per-object and batch-request layers. `Retry-After` is honored when
  the server pins a delay; otherwise exponential backoff applies.
- Range-resume on interrupted downloads: partial files persist at
  `.git/lfs/incomplete/<oid>.part` so the next attempt sends
  `Range: bytes=…` rather than re-fetching from byte 0.

Not yet here: tus uploads, custom transfer agents. Tracked in the
workspace `NOTES.md`.

Part of the [git-lfs Rust workspace](https://gitlab.com/rustutils/git-lfs).
Experimental — not yet ready for production. License: MIT.
