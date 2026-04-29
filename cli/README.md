# git-lfs

A from-scratch Rust port of [Git LFS](https://github.com/git-lfs/git-lfs).
The goal is feature parity with the upstream Go binary at the CLI and
wire-protocol level, with a cleaner library split and a better help
output in the binary.

## Status

**Experimental — not yet ready for production use.** About 290 of the
790 vendored upstream shell tests pass (~37%). Day-to-day flows —
clean / smudge, `fetch` / `pull` / `push`, `track`, locking, `migrate`
— work end to end against authenticated LFS endpoints, but several
commands are missing or partial (`env` output, `config`, custom
transfer adapters, SSH, retry/rate-limit handling, parts of `migrate`).
For real workloads, use upstream `git-lfs`.

## Why

This is a learning exercise and a parity-compatible base for
experimentation — for example, content-defined chunking on top of LFS
to reduce upload size when large files change. See the
[project README](https://gitlab.com/rustutils/git-lfs) for the longer
version.

## License

MIT, with attribution to the upstream Git LFS contributors.
