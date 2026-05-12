With `--recent` (or `lfs.fetchrecentalways=true`), fetch downloads objects from recently-active refs and commits in addition to the ones the named refs ask for. The idea is to pre-populate the cache so a later checkout or diff of "what we were working on last week" doesn't trigger another download round-trip.

What counts as 'recent' is controlled by these gitconfig keys:

- `lfs.fetchrecentrefsdays`: include branches whose tip commit is within this many days of now. Only local refs are scanned unless `lfs.fetchrecentremoterefs` is also set. Default 7.
- `lfs.fetchrecentremoterefs`: also scan the remote-tracking refs of the remote being fetched. Useful for picking up branches you might check out later without first creating a tracking local ref. Default true.
- `lfs.fetchrecentcommitsdays`: in addition to fetching the tip state of each recent ref, also fetch any LFS object referenced by commits within this many days of that ref's tip. Default 0 (tip only).
- `lfs.fetchrecentalways`: when true, always behave as if `--recent` was passed. Default false.
