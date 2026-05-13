# git-lfs-filter

Clean and smudge filters and the filter-process protocol for Git LFS.

Git invokes content filters whenever a file moves between the
working tree and a git blob: a *clean* filter runs on the way
in (`git add`) and a *smudge* filter runs on the way out
(`git checkout`). LFS hooks into both ends. Clean hashes the
working-tree bytes, hands them to the local LFS store, and
emits a small pointer file (which is what git ends up storing);
smudge takes the pointer back from git, looks up the real
bytes (fetching from the server if they're not local), and
writes the content into the working tree.

This crate implements both filters plus the long-running
[filter-process protocol][filter-process], which modern git
uses by default: one subprocess handles many files in a single
session over a pkt-line-framed connection. The three entry
points are `clean`, `smudge`, and `filter_process`, invoked as
the bodies of `git lfs clean`, `git lfs smudge`, and
`git lfs filter-process` respectively.

Pointer extensions chain external programs between the raw
bytes and the stored object. On clean, content passes through
each registered extension in priority order, with each stage's
input OID recorded in the resulting pointer; smudge undoes the
chain in reverse to reconstruct the original bytes. Used for
case-inverters, content-defined chunking, encryption shims, or
similar transforms; configured via `lfs.extension.<name>.*` in
git config.

[filter-process]: https://git-scm.com/docs/gitattributes#_long_running_filter_process
