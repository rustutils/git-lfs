# git-lfs-prune

## Name

`git-lfs-prune` — Delete old LFS files from local storage

## Synopsis

```
git-lfs-prune [OPTIONS]
```

## Description

Delete old LFS files from local storage

Delete locally stored LFS objects that aren't reachable from HEAD or any unpushed commit, freeing up disk space.

Note: many of upstream's prune options aren't yet supported — `--force`, `--recent`, `--verify-remote` (and the `--no-...` variants), `--verify-unreachable`, `--when-unverified`, the recent-refs / recent-commits retention windows, and the stash / worktree retention rules. The basic reachable-from-HEAD-or-unpushed walk is implemented and matches upstream's default semantics.

## Options

### Flags

- `-d`, `--dry-run`
    Don't actually delete anything; just report what would have been done

- `-v`, `--verbose`
    Report the full detail of what is/would be deleted

## See also

[git-lfs-fetch(1)](./git-lfs-fetch.md), [gitignore(5)](https://git-scm.com/docs/gitignore).

## Reporting bugs

This command is from the Rust implementation of git-lfs, not the original
Go implementation. Please report bugs to our [issue tracker](https://gitlab.com/rustutils/git-lfs/issues).
