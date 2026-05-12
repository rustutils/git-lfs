Deletes local copies of LFS files that have aged out and are no longer needed, freeing disk space. Prune walks the local object store and removes anything not retained by at least one of:

- HEAD's tree in the current checkout
- HEAD's tree in any linked worktree (`git worktree`)
- The stash
- A 'recent ref' — see RECENT FILES
- A 'recent commit' on HEAD or any recent ref — see RECENT FILES
- An unpushed commit — see UNPUSHED LFS FILES

In short: prune deletes objects you aren't currently using and that aren't 'recent', as long as they've been pushed. The reflog isn't consulted, so LFS objects only reachable from orphaned commits are always deleted.

`lfs.fetchexclude` / `lfs.fetchinclude` (comma-separated `gitignore`-style patterns) restrict which paths each retention producer scans. See git-lfs-config(5).

Note: don't run `git lfs prune` when multiple repositories share a custom storage directory. See `lfs.storage` in git-lfs-config(5) for the implications.
