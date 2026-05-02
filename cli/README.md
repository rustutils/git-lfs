# git-lfs

A from-scratch Rust port of [Git LFS](https://github.com/git-lfs/git-lfs).
The goal is feature parity with the upstream Go binary at the CLI and
wire-protocol level, with a cleaner library split and a better help
output in the binary.

## Status

This crate is currently experimental. It passes 75% of the upstream
test suite, so it should be functional for day-to-day use. But it is
not as battle-tested as the upstream Go git-lfs, and there is functionality
which has not been implemented yet, specifically:

- `git lfs config`
- custom transfer adapters
- SSH `git-lfs-authenticate`
- `merge-driver`
- `dedup`
- Retry/Retry-After handling

For production workloads, use upstream `git-lfs`.

## Why

`git-lfs` is a good codebase to reimplement in Rust. It is well-specified,
the upstream test suite is comprehensive (kudos to the original authors
on their testing discipline), and it is widely used. Reimplementing
real-world tooling in Rust is a great learning exercise, and this can
slot into the existing work with `gix`.

## License

MIT, with attribution to the upstream Git LFS contributors.
