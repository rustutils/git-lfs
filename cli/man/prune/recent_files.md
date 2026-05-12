Prune keeps LFS files referenced by 'recent' commits so you can switch back to them without re-downloading. 'Recent' has the same meaning here as for `git lfs fetch --recent`, with an extra offset (default 3 days) so files you downloaded recently stick around for a while.

Settings:

- `lfs.pruneoffsetdays`: extra days added to the fetch-recent windows. A ref or commit has to be at least this many days older than the oldest one `--recent` would download for prune to consider it old enough to delete. Default 3. Only takes effect when the underlying `lfs.fetchrecent*days` setting is non-zero.
- `lfs.fetchrecentrefsdays`, `lfs.fetchrecentremoterefs`, `lfs.fetchrecentcommitsdays`: same meaning as in git-lfs-fetch(1), used as the base for the offset above. A day value of 0 disables that retention dimension entirely (everything outside the other rules becomes prunable).
