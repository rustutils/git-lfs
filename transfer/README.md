# git-lfs-transfer

Concurrent transfer queue and basic adapter for Git LFS uploads and downloads.

When Git LFS wants to transfer files between a client and a
server, it first asks the server's batch endpoint with a list of
OIDs and sizes for the files involved, and the server returns one
URL per object (typically a presigned link to S3 or a CDN, plus
auth headers and an expiry window); the client then PUTs or GETs
the bytes against those URLs.

This crate implements the client side of that dance. It sits between
[git-lfs-api] and [git-lfs-store]: given a list of `(oid, size)`
pairs, it negotiates the batch, drives the per-object byte
movement concurrently, and streams progress events back to the
caller.

A bounded pool runs up to `concurrency` transfers in flight at once.
Each transfer streams through the network rather than buffering the
whole object in memory, and downloads hash-verify against the OID
the server promised before committing into the store.

Failures are localized: one corrupt object doesn't tear down the
queue. Retries handle transient errors (5xx, 429, network blips)
with exponential backoff, or the server's `Retry-After` when one is
supplied. Expired action URLs are caught before dialing rather than
burning a request on a guaranteed-to-fail endpoint, and interrupted
downloads resume via `Range:` from a `.part` file rather than
restarting at byte 0.

Only the `basic` HTTPS transfer is implemented at the moment. The
`tus`, custom-transfer-agent, and pure-SSH adapters are not
implemented yet.

[git-lfs-api]: https://crates.io/crates/git-lfs-api
[git-lfs-store]: https://crates.io/crates/git-lfs-store
