# git-lfs-ext-list

## Name

`git-lfs-ext-list` — List configured LFS pointer extensions, optionally filtered by name

## Synopsis

```
git-lfs-ext-list [NAMES]...
```

## Description

List configured LFS pointer extensions, optionally filtered by name

## Options

### Arguments

- `<NAMES>`
    Extension names to print. With no names, prints all configured extensions (same as bare `git lfs ext`)

## Reporting bugs

This command is from the Rust implementation of git-lfs, not the original
Go implementation. Please report bugs to our [issue tracker](https://gitlab.com/rustutils/git-lfs/issues).
