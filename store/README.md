# git-lfs-store

Content-addressable on-disk store for Git LFS objects.

Git LFS keeps large files outside git's object database, leaving
small pointer blobs committed to git in their place. This crate owns
the local half of that split: where the actual file bytes live on
disk, how they get there, and how they're served back out.

Bytes are stored on disk in a sharded tree under `.git/lfs/objects/`
keyed by SHA-256 — same layout upstream `git-lfs` uses. Inserts go
through a tempfile rename and hash content as they write it, so an
interrupted write never leaves a half-committed file.

Downloads stream to `.git/lfs/incomplete/<oid>.part` and only rename
into place once complete. An interrupted transfer leaves its partial
bytes on disk so the next attempt resumes via `Range:` instead of
restarting.

Alternate object stores attach as fallback read sources — a missed
lookup hardlinks or copies the object in from the first alternate
that has it, the LFS analogue of `.git/objects/info/alternates`.

File and directory modes follow `core.sharedRepository`, so a shared
repo gets the same permission scheme git itself would produce.
