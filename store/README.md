# git-lfs-store

Content-addressable on-disk store for Git LFS objects.

Implements the layout `git-lfs` uses to keep the actual bytes that
LFS pointers refer to: a sharded directory tree under `.git/lfs/objects/`
keyed by SHA-256, with atomic insert via a tempfile rename. A file's
existence and size double as its integrity check; the OID is
re-verified on insert.

In-progress downloads stage at `<root>/incomplete/<oid>.part` via
`incomplete_path` + `commit_partial`. Bytes from an interrupted
transfer stay on disk so the next attempt can resume with a `Range:`
request instead of starting over.

```rust
use git_lfs_store::Store;

let store = Store::new(".git/lfs");
let (oid, size) = store.insert(&mut "hello world".as_bytes()).unwrap();
assert!(store.contains_with_size(oid, size));
let mut reader = store.open(oid).unwrap();
```

Part of the [git-lfs Rust workspace](https://gitlab.com/rustutils/git-lfs).
Experimental — not yet ready for production. License: MIT.
