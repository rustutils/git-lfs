# git-lfs-push

## Name

`git-lfs-push` — Upload every LFS object reachable from the given refs that the remote doesn't already have. The "doesn't have" set is approximated by `refs/remotes/<remote>/*`; the LFS server's batch API also dedupes server-side so missing exclusions don't waste bandwidth

## Synopsis

```
git-lfs-push [OPTIONS] <REMOTE> [ARGS]...
```

## Description

Upload every LFS object reachable from the given refs that the remote doesn't already have. The "doesn't have" set is approximated by `refs/remotes/<remote>/*`; the LFS server's batch API also dedupes server-side so missing exclusions don't waste bandwidth

## Options

### Arguments

- `<REMOTE>`
    Name of the remote (e.g. "origin") whose tracking refs are excluded from the upload set

- `<ARGS>`
    Refs (or, with `--object-id`, raw OIDs) to push. With `--all`, restricts the all-refs walk to these; with `--stdin`, ignored (a warning is emitted)

### Flags

- `--dry-run`
    List the objects that would be pushed without actually uploading them (one `push <oid> => <path>` line per object)

- `--all`
    Push every local ref under `refs/heads/*` and `refs/tags/*` (intersected with `args` if any are given)

- `--stdin`
    Read refs (or OIDs, with `--object-id`) from stdin, one per line. Blank lines are skipped

- `--object-id`
    Treat positional args / stdin entries as raw LFS OIDs rather than git refs, and upload those objects directly from the local store

