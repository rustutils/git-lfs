# git-lfs-ext

## Name

`git-lfs-ext` — List the configured LFS pointer extensions (`lfs.extension.<name>.*`). Extensions chain external clean/smudge programs around each LFS object; this prints their resolved configuration in priority order

## Synopsis

```
git-lfs-ext
```

## Description

List the configured LFS pointer extensions (`lfs.extension.<name>.*`). Extensions chain external clean/smudge programs around each LFS object; this prints their resolved configuration in priority order

## Reporting bugs

This command is from the Rust implementation of git-lfs, not the original
Go implementation. Please report bugs to our [issue tracker](https://gitlab.com/rustutils/git-lfs/issues).
