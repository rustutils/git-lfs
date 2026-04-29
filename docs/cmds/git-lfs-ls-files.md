# git-lfs-ls-files

## Name

`git-lfs-ls-files` — List LFS-tracked files visible at a ref (default: HEAD), or across all reachable history with `--all`

## Synopsis

```
git-lfs-ls-files [OPTIONS] [REFSPEC]
```

## Description

List LFS-tracked files visible at a ref (default: HEAD), or across all reachable history with `--all`

## Options

### Arguments

- `<REFSPEC>`
    Ref to list. Defaults to HEAD

### Flags

- `-l`, `--long`
    Show full 64-char OID instead of the 10-char prefix

- `-s`, `--size`
    Append humanized size in parens

- `-n`, `--name-only`
    Print only the path

- `-a`, `--all`
    Walk every reachable ref's full history

- `-d`, `--debug`
    Multi-line per-file block (size, checkout, download, oid, version)

- `-j`, `--json`
    Stable JSON output for scripts

