# git-lfs-filter

Clean and smudge filters, plus the long-running [filter-process
protocol](https://git-scm.com/docs/gitattributes#_long_running_filter_process),
for Git LFS.

This crate implements the three things git invokes when a user runs
`git add` or `git checkout` on an LFS-tracked file:

- **`clean`** — read raw file content on stdin, hash it, store the
  bytes locally, emit a pointer file on stdout. Used on `git add`.
- **`smudge`** — read a pointer on stdin, look up the bytes locally
  (or fetch them on demand), emit raw content on stdout. Used on
  `git checkout`.
- **`filter-process`** — the modern long-running variant of the same
  protocol. One subprocess handles many files in one session,
  speaking pkt-line framing.

Designed to be run as the body of `git-lfs clean`, `git-lfs smudge`,
and `git-lfs filter-process` — the entry points wired up by
`git lfs install` via `filter.lfs.{clean,smudge,process}` config.

Part of the [git-lfs Rust workspace](https://gitlab.com/rustutils/git-lfs).
Experimental — not yet ready for production. License: MIT.
