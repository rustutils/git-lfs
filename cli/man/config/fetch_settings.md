- `lfs.fetchinclude`

  Comma-separated list of `gitignore(5)`-style patterns. When set, fetch only downloads objects whose path matches one of them. Empty string disables the filter.

- `lfs.fetchexclude`

  Inverse of `fetchinclude` — fetch skips objects whose path matches.

- `lfs.fetchrecentrefsdays`

  Branches whose tip commit lies within this many days of now are included by `fetch --recent`. Only local refs are scanned unless `lfs.fetchrecentremoterefs` is also set. Default 7. A value of 0 disables ref-window retention entirely.

- `lfs.fetchrecentremoterefs`

  When `true`, `fetch --recent` also scans the remote-tracking refs of the remote being fetched (useful for picking up branches you might check out later without first creating a tracking local ref). Default `true`.

- `lfs.fetchrecentcommitsdays`

  In addition to fetching the tip state of each recent ref, also fetch LFS objects referenced by commits within this many days of that ref's tip. Default 0 (tip only).

- `lfs.fetchrecentalways`

  When `true`, always behave as if `--recent` was passed. Default `false`.
