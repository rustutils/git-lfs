# git-lfs-locks

## Name

`git-lfs-locks` — List file locks held on the server

## Synopsis

```
git-lfs-locks [OPTIONS]
```

## Description

List file locks held on the server

## Options

### Flags

- `-r`, `--remote` `<REMOTE>`
    Specify which remote to use when interacting with locks

- `-p`, `--path` `<PATH>`
    Filter results to a particular path

- `-i`, `--id` `<ID>`
    Filter results to a particular lock id

- `-l`, `--limit` `<LIMIT>`
    Maximum number of results to return

- `--ref` `<REFSPEC>`
    Refspec to filter locks by (defaults to current branch / tracked upstream — same auto-resolution as `git lfs lock`)

- `--verify`
    Verify ownership: prefix locks owned by the authenticated user with `O ` (others get `  `)

- `--local`
    List from the on-disk cache of own locks instead of querying the server. Combine with `--path` / `--id` / `--limit` to filter; `--verify` is rejected. Useful when offline or to confirm what `git lfs lock` recorded locally

- `-j`, `--json`
    Stable JSON output for scripts

