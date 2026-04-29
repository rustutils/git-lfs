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

Not yet here: tus uploads, custom transfer agents, retry/Retry-After
handling. Tracked in `NOTES.md` upstream.

Part of the [git-lfs Rust workspace](https://gitlab.com/rustutils/git-lfs).
Experimental — not yet ready for production. License: MIT.
