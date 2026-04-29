# git-lfs-pointer

Parse and encode [Git LFS pointer files](https://github.com/git-lfs/git-lfs/blob/main/docs/spec.md).

A pointer is a small UTF-8 text file that stands in for a large file in
a git repo. It carries the file's SHA-256 OID, its size, and an
optional list of extension records. This crate is a self-contained
parser/encoder for that format — no I/O, no network, no git.

```rust
use git_lfs_pointer::{Oid, Pointer};

let oid: Oid = "4d7a214614ab2935c943f9e0ff69d22eadbb8f32b1258daaa5e2ca24d17e2393"
    .parse()
    .unwrap();
let pointer = Pointer::new(oid, 12345);

let encoded = pointer.encode();
let parsed = Pointer::parse(encoded.as_bytes()).unwrap();
assert_eq!(parsed.oid, oid);
assert_eq!(parsed.size, 12345);
assert!(parsed.canonical);
```

Part of the [git-lfs Rust workspace](https://gitlab.com/rustutils/git-lfs).
Experimental — not yet ready for production. License: MIT.
