# git-lfs-ext

## Name

`git-lfs-ext` — List the configured LFS pointer extensions

## Synopsis

```
git-lfs-ext
```

## Description

List the configured LFS pointer extensions

Print each `lfs.extension.<name>.*` entry resolved to its final configuration in priority order. Extensions chain external clean / smudge programs around each LFS object — see [git-lfs-config(5)](./git-lfs-config.md) for how to configure them.

## Reporting bugs

This command is from the Rust implementation of git-lfs, not the original
Go implementation. Please report bugs to our [issue tracker](https://gitlab.com/rustutils/git-lfs/issues).
