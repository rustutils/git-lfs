# git-lfs-fetch

## Name

`git-lfs-fetch` — Download every LFS object reachable from the given refs (default: HEAD) that isn't already in the local store. Walks history, dedupes by OID

## Synopsis

```
git-lfs-fetch [OPTIONS] [ARGS]...
```

## Description

Download every LFS object reachable from the given refs (default: HEAD) that isn't already in the local store. Walks history, dedupes by OID

## Options

### Arguments

- `<ARGS>`
    First positional arg is treated as a remote name (if it resolves); subsequent args are refs

### Flags

- `--dry-run`
    List the objects that would be fetched without downloading them (one `fetch <oid> => <path>` line per object)

- `--json`
    JSON output. With `--dry-run`, queries the server's batch endpoint to populate `actions` URLs

- `--all`
    Walk every local ref under `refs/heads/*` + `refs/tags/*`

- `--refetch`
    Re-download objects we already have (e.g. recovery from a corrupt local store)

- `--stdin`
    Read refs from stdin, one per line. Blank lines dropped

- `--prune`
    Run `prune` after the fetch completes

- `-I`, `--include` `<INCLUDE>`
    Comma-separated globs; only matching paths are fetched. Falls back to `lfs.fetchinclude` when omitted

- `-X`, `--exclude` `<EXCLUDE>`
    Comma-separated globs; matching paths are skipped. Falls back to `lfs.fetchexclude` when omitted

