# git-lfs-pointer

## Name

`git-lfs-pointer` — Build, compare, and check pointers

## Synopsis

```
git-lfs-pointer [OPTIONS]
```

## Description

Build, compare, and check pointers

Build and optionally compare generated pointer files to ensure consistency between different Git LFS implementations.

## Options

### Flags

- `-f`, `--file` `<FILE>`
    A local file to build the pointer from

- `-p`, `--pointer` `<POINTER>`
    A local file containing a pointer generated from another implementation.

    Compared to the pointer generated from `--file`.

- `--stdin`
    Read the pointer from standard input to compare with the pointer generated from `--file`

- `--check`
    Read the pointer from standard input (with `--stdin`) or the filepath (with `--file`).

    If neither or both of `--stdin` and `--file` are given, the invocation is invalid. Exits 0 if the data read is a valid Git LFS pointer, 1 otherwise. With `--strict`, exits 2 if the pointer is not byte-canonical.

- `--strict`
    With `--check`, verify that the pointer is canonical (the one Git LFS would create).

    If it isn't, exits 2. The default — for backwards compatibility — is `--no-strict`.

- `--no-strict`
    Disable strict mode (paired with `--strict`)

- `--no-extensions`
    Build a plain pointer without running configured `lfs.extension.*` clean commands. Default behavior is to chain through any extensions (and emit a `warning:` line on stderr); pass this to suppress both the chain and the warning

## Reporting bugs

This command is from the Rust implementation of git-lfs, not the original
Go implementation. Please report bugs to our [issue tracker](https://gitlab.com/rustutils/git-lfs/issues).
