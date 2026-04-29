# git-lfs-unlock

## Name

`git-lfs-unlock` — Release a file lock previously acquired with `git lfs lock`. Either provide one or more paths, or `--id <id>` (mutually exclusive)

## Synopsis

```
git-lfs-unlock [OPTIONS] [PATHS]...
```

## Description

Release a file lock previously acquired with `git lfs lock`. Either provide one or more paths, or `--id <id>` (mutually exclusive)

## Options

### Arguments

- `<PATHS>`
    Paths to unlock; mutually exclusive with `--id`

### Flags

- `-i`, `--id` `<ID>`
    Lock id to release; mutually exclusive with paths

- `-f`, `--force`
    Forcibly break another user's lock(s)

- `-r`, `--remote` `<REMOTE>`
    Specify which remote to use when interacting with locks

- `--ref` `<REFSPEC>`
    Refspec to send with the unlock request (defaults to current branch / tracked upstream)

- `-j`, `--json`
    Stable JSON output for scripts

