// Standalone module for the Go helpers the upstream shell tests use, such
// as the credential helper `git-credential-lfstest`. They're vendored from
// upstream Git LFS v3.8.1 (`t/cmd/`) unchanged, so the tests behave as
// upstream's do.
//
// Helpers that import upstream's internal packages can't be built here:
// `lfstest-customadapter`, `lfstest-standalonecustomadapter`, and
// `lfstest-testutils` (ported to Rust in `src/bin/`) are left out, and so
// is `lfstest-gitserver` (ported in `../server/`).
// `lfstest-getlimit` only needed two constants from them, which it now
// defines itself.

module github.com/xfbs/git-lfs-rust/tests/cmd

go 1.24

require (
	github.com/git-lfs/pktline v0.0.0-20210330133718-06e9096e2825
	github.com/google/shlex v0.0.0-20191202100458-e7afc7fbc510
)
