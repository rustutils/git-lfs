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

## Options

### Flags

- `-d`, `--dry-run`
    Don't actually delete anything; just report what would have been done

- `-v`, `--verbose`
    Report the full detail of what is/would be deleted

- `--recent`
    Ignore the recent-refs / recent-commits retention windows when computing what is prunable. Equivalent to setting `lfs.fetchrecentrefsdays` and `lfs.fetchrecentcommitsdays` to 0 for this invocation

- `-f`, `--force`
    Treat every pushed object as prunable regardless of the recent-refs / recent-commits / unpushed retention rules. Pointers reachable from HEAD's tree are still kept

- `-c`, `--verify-remote`
    Verify with the remote that prunable objects exist there before deleting them locally. With this on, an object that can't be served by the remote either halts the prune (default) or is dropped from the delete set (`--when-unverified=continue`). Reachable-but-unverified objects are reported as `missing on remote:`; unreachable objects (orphans not in any commit) are silently passed through unless `--verify-unreachable` is also set. Overrides `lfs.pruneverifyremotealways`

- `--no-verify-remote`
    Override `lfs.pruneverifyremotealways=true` and skip the remote verify pass for this invocation

- `--verify-unreachable`
    When `--verify-remote` is in effect, verify orphan objects (not reachable from any commit) too. Without this, orphans pass through verification silently and are still pruned. Overrides `lfs.pruneverifyunreachablealways`

- `--no-verify-unreachable`
    Override `lfs.pruneverifyunreachablealways=true` and skip orphan verification for this invocation

- `--when-unverified` `<MODE>`
    What to do when `--verify-remote` finds objects missing on the remote. `halt` (the default) refuses the prune and lists the missing OIDs; `continue` drops them from the delete set and prunes the verified ones

## See also

[git-lfs-fetch(1)](./git-lfs-fetch.md), [gitignore(5)](https://git-scm.com/docs/gitignore).

## Reporting bugs

This command is from the Rust implementation of git-lfs, not the original
Go implementation. Please report bugs to our [issue tracker](https://gitlab.com/rustutils/git-lfs/issues).
