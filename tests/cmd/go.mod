// Standalone module for the integration-test helper binaries.
//
// The helpers are vendored verbatim from the upstream git-lfs Go
// project (commands/t/cmd/) — they implement the mock LFS server
// (`lfstest-gitserver`) and supporting tools (`lfstest-count-tests`
// etc.) that the upstream shell tests under `tests/t-*.sh` rely on.
// We use them as-is so the tests behave exactly like upstream's,
// surfacing any divergence in our Rust binary's behavior rather
// than in re-implemented test plumbing.
//
// This module is intentionally separate from anything in our
// workspace — it has no Rust counterpart and only exists at test
// time. A future `cargo test`-native rewrite (planned in NOTES.md)
// would retire this module entirely.
//
// Helpers that import upstream-internal packages
// (`lfstest-customadapter`, `lfstest-standalonecustomadapter`,
// `lfstest-testutils`) are excluded from the build — the tests
// that exercise them are documented as skipped in NOTES.md.

module github.com/xfbs/git-lfs-rust/tests/cmd

go 1.24

require github.com/klauspost/compress v1.18.5
