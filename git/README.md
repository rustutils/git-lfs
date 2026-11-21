# git-lfs-git

Git helpers for Git LFS: config, refs, scanners, and `.gitattributes` matching.

Git LFS asks Git about the repository it's running in: where it lives, what its
config says, which objects each ref reaches, and which attributes apply to a
path. This crate does that by running the `git` binary the user has installed.
It doesn't contain its own Git implementation.

The helpers are independent of each other, so you can use only the parts you
need:

- Paths: find the Git directory, the working tree, and the LFS directory,
  including in linked worktrees.
- Config: read and write Git config at every scope, including `.lfsconfig`,
  URL-specific settings, and `url.<base>.insteadOf` aliases.
- Endpoints: find a remote's LFS server URL the same way upstream does.
- Refs and history: resolve refspecs, walk commits, and find the LFS pointers
  reachable from a set of refs.
- Objects: long-running `git cat-file` processes for reading blobs, and a
  parser for `git diff-index` output.
- Attributes: a `.gitattributes` parser and matcher, built on gix-attributes
  and gix-glob.
- Packet lines: the framing used by Git's long-running filter protocol.
