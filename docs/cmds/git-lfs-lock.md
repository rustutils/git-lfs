# git-lfs-lock

## Name

`git-lfs-lock` — Acquire an exclusive server-side lock on one or more files. Other users will be unable to push changes to a locked file

## Synopsis

```
git-lfs-lock [OPTIONS] [PATHS]...
```

## Description

Acquire an exclusive server-side lock on one or more files. Other users will be unable to push changes to a locked file

## Options

### Arguments

- `<PATHS>`
    Paths to lock (repo-relative or absolute, must resolve inside the working tree)

### Flags

- `-r`, `--remote` `<REMOTE>`
    Specify which remote to use when interacting with locks

- `--ref` `<REFSPEC>`
    Refspec to associate the lock with. Defaults to the current branch's tracked upstream (`branch.<current>.merge`) or the current branch's full ref (`refs/heads/<branch>`)

- `-j`, `--json`
    Stable JSON output for scripts

