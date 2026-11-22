# git-lfs-dedup

## Name

`git-lfs-dedup` — Deduplicate LFS files in the working tree

## Synopsis

```
git-lfs-dedup [OPTIONS]
```

## Description

Deduplicate LFS files in the working tree

Replace each LFS file in the working tree with a copy-on-write clone of its object in the local store, so the two share disk space until one of them changes. This needs a file system that supports cloning, such as Btrfs, XFS, APFS, or ReFS.

The working tree must be clean, and no pointer extensions may be configured, since their files differ from the stored objects.

## Options

### Flags

- `-t`, `--test`
    Only check whether this system and repository support deduplication

## Reporting bugs

This command is from the Rust implementation of git-lfs, not the original
Go implementation. Please report bugs to our [issue tracker](https://gitlab.com/rustutils/git-lfs/issues).
