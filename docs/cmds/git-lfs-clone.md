# git-lfs-clone

## Name

`git-lfs-clone` — Deprecated. Wraps `git clone` so the working tree is populated with pointer text first, then runs `git lfs pull` to download LFS content in batch. Modern `git clone` parallelizes the smudge filter and is no slower; prefer it

## Synopsis

```
git-lfs-clone [ARGS]...
```

## Description

Deprecated. Wraps `git clone` so the working tree is populated with pointer text first, then runs `git lfs pull` to download LFS content in batch. Modern `git clone` parallelizes the smudge filter and is no slower; prefer it

## Options

### Arguments

- `<ARGS>`
    `git clone` and LFS pass-through args. The repository URL is required; an optional target directory follows

## Reporting bugs

This command is from the Rust implementation of git-lfs, not the original
Go implementation. Please report bugs to our [issue tracker](https://gitlab.com/rustutils/git-lfs/issues).
