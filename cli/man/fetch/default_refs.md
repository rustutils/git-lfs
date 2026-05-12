If no refs are given as arguments, the currently checked out ref is
used.

Pass `--recent` (or set `lfs.fetchrecentalways=true`) to also fetch
recently-touched refs and the recent pre-images on each. The window
is controlled by `lfs.fetchrecentrefsdays`, `lfs.fetchrecentremoterefs`,
and `lfs.fetchrecentcommitsdays`. See git-lfs-config(5).
