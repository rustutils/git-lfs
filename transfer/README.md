# git-lfs-transfer

Concurrent transfer queue with HTTPS and pure-SSH adapters for Git LFS uploads
and downloads.

To transfer files, a Git LFS client first sends the server a list of objects,
and gets back a URL for each one. It then uploads or downloads each object at
its URL. This crate does both steps: given a list of objects, it runs the batch
request through [git-lfs-api], transfers the objects in parallel, and reports
progress as it goes. Downloads go into [git-lfs-store].

Objects stream through without being held in memory, and downloads are checked
against their OID before they're stored. A failed object doesn't stop the
others. Rate limits, server errors, and network errors are retried with
backoff, or after the server's Retry-After delay. Expired URLs are caught
before they're used, and interrupted downloads resume where they stopped.

Two transfer methods are implemented: HTTPS, and the pure-SSH protocol of
`git-lfs-transfer`, which runs batch requests, transfers, and locking over SSH.
SSH remotes try pure SSH first, and fall back to HTTPS through
`git-lfs-authenticate` if the server doesn't support it. The tus and custom
transfer adapters aren't implemented yet.

[git-lfs-api]: https://crates.io/crates/git-lfs-api [git-lfs-store]:
https://crates.io/crates/git-lfs-store
