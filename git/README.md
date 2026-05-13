# git-lfs-git

Git interop helpers for Git LFS: config, refs, scanners, and `.gitattributes` matching.

Git LFS needs the user's git binary for a handful of things
with no LFS-specific equivalent: where the repo lives, what's
in its config, which objects each ref reaches, and how
`.gitattributes` applies to a given path. This crate collects
those helpers in one place. Everything runs by shelling out
to the `git` binary the user has installed; this crate does
not bundle its own git implementation.

It sits at the bottom of the LFS workspace: every other crate
(api, transfer, creds, filter, store) goes through it
whenever it needs to know something about the repo it's
running against. The crate is intentionally a collection of
unrelated helpers rather than a single abstraction, so the
pieces are independent of each other; pick what you need.

This crate implements helpers related to:

- Path: locate `.git/`, the work tree, and `.git/lfs/` for a
  path, including multi-worktree edge cases.
- Config: read and write Git configuration at local, global,
  system, and worktree scope, plus the `.lfsconfig` overlay
  and specific surfaces like `http.<url>.*` HTTP options,
  `url.<base>.insteadOf` URL aliases, `lfs.extension.<name>.*`
  pointer extensions, and the `lfs.fetchrecent*` and
  `lfs.prune*` windows.
- Endpoint: walk the full upstream priority chain
  (`GIT_LFS_URL` → `lfs.url` → `remote.<name>.lfsurl` →
  derived from `remote.<name>.url`, with SSH-to-HTTPS
  rewriting) to find the LFS server URL for a remote.
- Refs and history: resolve refspecs and tracking branches,
  walk commits with `rev-list`, and enumerate LFS pointer
  blobs reachable from a set of refs (drives `fetch`,
  `pull`, `push`, `prune`).
- Object access: long-lived `cat-file --batch` and
  `--batch-check` workers for blob inspection, plus `git
  diff-index` output parsing for pre-push.
- Attributes: a `.gitattributes` parser and matcher backed
  by `gix-attributes` and `gix-glob`.
- Packet-line framing: the pkt-line protocol used by
  filter-process to multiplex many filter operations in one
  subprocess.
