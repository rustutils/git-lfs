# git-lfs-filter-process

## Name

`git-lfs-filter-process` — Run the long-running filter-process protocol with git over stdin/stdout. This is what git invokes via filter.lfs.process and is the batched alternative to per-invocation `clean`/`smudge`

## Synopsis

```
git-lfs-filter-process [OPTIONS]
```

## Description

Run the long-running filter-process protocol with git over stdin/stdout. This is what git invokes via filter.lfs.process and is the batched alternative to per-invocation `clean`/`smudge`

## Options

### Flags

- `--skip`
    Pass smudge requests' pointer text through unchanged; equivalent to `GIT_LFS_SKIP_SMUDGE=1`. Wired up by `install --skip-smudge`

## Reporting bugs

This command is from the Rust implementation of git-lfs, not the original
Go implementation. Please report bugs to our [issue tracker](https://gitlab.com/rustutils/git-lfs/issues).
