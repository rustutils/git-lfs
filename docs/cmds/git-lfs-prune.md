# git-lfs-prune

## Name

`git-lfs-prune` — Delete old LFS files from local storage

## Synopsis

```
git-lfs-prune [OPTIONS]
```

## Description

Deletes local copies of LFS files that have aged out and are no longer needed, freeing disk space. Prune walks the local object store and removes anything not retained by at least one of:

- HEAD's tree in the current checkout
- HEAD's tree in any linked worktree (`git worktree`)
- The stash
- A 'recent ref' — see RECENT FILES
- A 'recent commit' on HEAD or any recent ref — see RECENT FILES
- An unpushed commit — see UNPUSHED LFS FILES

In short: prune deletes objects you aren't currently using and that aren't 'recent', as long as they've been pushed. The reflog isn't consulted, so LFS objects only reachable from orphaned commits are always deleted.

`lfs.fetchexclude` / `lfs.fetchinclude` (comma-separated `gitignore`-style patterns) restrict which paths each retention producer scans. See [git-lfs-config(5)](./git-lfs-config.md).

Note: don't run `git lfs prune` when multiple repositories share a custom storage directory. See `lfs.storage` in [git-lfs-config(5)](./git-lfs-config.md) for the implications.

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

## Recent files

Prune keeps LFS files referenced by 'recent' commits so you can switch back to them without re-downloading. 'Recent' has the same meaning here as for `git lfs fetch --recent`, with an extra offset (default 3 days) so files you downloaded recently stick around for a while.

Settings:

- `lfs.pruneoffsetdays`: extra days added to the fetch-recent windows. A ref or commit has to be at least this many days older than the oldest one `--recent` would download for prune to consider it old enough to delete. Default 3. Only takes effect when the underlying `lfs.fetchrecent*days` setting is non-zero.
- `lfs.fetchrecentrefsdays`, `lfs.fetchrecentremoterefs`, `lfs.fetchrecentcommitsdays`: same meaning as in [git-lfs-fetch(1)](./git-lfs-fetch.md), used as the base for the offset above. A day value of 0 disables that retention dimension entirely (everything outside the other rules becomes prunable).

## Unpushed lfs files

LFS files reachable from a commit that hasn't reached the remote are never pruned, regardless of age — the local copy is the only one.

'Pushed' is determined by comparing local refs against the remote's refs: any LFS file referenced by a commit reachable from a local ref but not from the corresponding remote ref is treated as unpushed. The pre-push hook uploads LFS objects before the remote branch updates, so this comparison gives an accurate picture.

See DEFAULT REMOTE for which remote anchors the comparison.

## Verify remote

`--verify-remote` (`-c`) asks the remote whether every prunable LFS file has a server-side copy before deleting it locally. The UNPUSHED LFS FILES check above is usually enough, but `--verify-remote` adds belt-and-braces for cases where you want to be sure (at the cost of extra batch calls to the server).

Enable as the default by setting `lfs.pruneverifyremotealways=true`.

`--verify-unreachable` extends the verification pass to LFS objects that aren't referenced by any commit (orphans — added to the index but never committed, or referenced only by orphaned commits). Without this flag, orphans pass through `--verify-remote` silently and are deleted. Enable as the default with `lfs.pruneverifyunreachablealways=true`.

By default, `--verify-remote` halts the entire prune if any object can't be verified. Pass `--when-unverified=continue` to instead drop the unverifiable objects from the delete set and proceed with the rest.

See DEFAULT REMOTE for which remote is queried.

## Default remote

`origin` is the default remote consulted for UNPUSHED LFS FILES and VERIFY REMOTE. Even with multiple remotes configured, prune treats this one as canonical — usually it's the main central repo (or your fork of it), and a valid backup of your work.

If `origin` isn't configured, prune treats every reachable LFS file as unpushed and effectively retains everything.

Override the canonical remote with `lfs.pruneremotetocheck`: set it to a different remote name to anchor against that one instead.

## See also

[git-lfs-fetch(1)](./git-lfs-fetch.md), [gitignore(5)](https://git-scm.com/docs/gitignore).

## Reporting bugs

This command is from the Rust implementation of git-lfs, not the original
Go implementation. Please report bugs to our [issue tracker](https://gitlab.com/rustutils/git-lfs/issues).
