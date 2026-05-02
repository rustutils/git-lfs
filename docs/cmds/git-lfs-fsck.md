# git-lfs-fsck

## Name

`git-lfs-fsck` — Check the integrity of LFS objects and pointers reachable from `<refspec>` (default: HEAD). Exit 1 if anything is corrupt

## Synopsis

```
git-lfs-fsck [OPTIONS] [REFSPEC]
```

## Description

Check the integrity of LFS objects and pointers reachable from `<refspec>` (default: HEAD). Exit 1 if anything is corrupt

## Options

### Arguments

- `<REFSPEC>`
    Ref to scan. Defaults to HEAD

### Flags

- `--objects`
    Only check objects (verify store contents match pointer OIDs)

- `--pointers`
    Only check pointers (flag non-canonical pointer encodings)

- `-d`, `--dry-run`
    Report problems but don't move corrupt objects to `<lfs>/bad/`

## Reporting bugs

This command is from the Rust implementation of git-lfs, not the original
Go implementation. Please report bugs to our [issue tracker](https://gitlab.com/rustutils/git-lfs/issues).
