# git-lfs-git

Git interop helpers for [Git LFS](https://gitlab.com/rustutils/git-lfs).

This crate is the "talk to `git`" layer of the workspace: helpers that
shell out to the `git` binary the user has installed, plus parsers for
the file formats git emits. It does not bundle its own git
implementation.

What's here:

- **`config`** — read/write `git config` at local / global / system /
  worktree scope, plus `.lfsconfig` overlay.
- **`endpoint`** — resolve the LFS server URL via the full upstream
  priority chain (`GIT_LFS_URL` → `lfs.url` → `remote.<name>.lfsurl`
  → derived from `remote.<name>.url`, with SSH→HTTPS rewriting).
- **`refs`** — current ref / refspec / tracking-branch resolution.
- **`scanner`** — `rev_list` + `cat-file --batch[-check]` driven walks
  to enumerate LFS pointers reachable from a set of refs (used by
  `fetch` / `pull` / `push`).
- **`scan_tree`** — single-tree variant for `ls-files` and `status`.
- **`diff_index`** — parser for `git diff-index` output, used by
  `pre-push`.
- **`attr`** — `.gitattributes` parser + matcher backed by
  `gix-attributes` and `gix-glob`.

Part of the [git-lfs Rust workspace](https://gitlab.com/rustutils/git-lfs).
Experimental — not yet ready for production. License: MIT.
