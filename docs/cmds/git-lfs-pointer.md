# git-lfs-pointer

## Name

`git-lfs-pointer` — Debug helper: build a pointer from a file, parse one from disk or stdin, or just check whether some bytes are a valid pointer

## Synopsis

```
git-lfs-pointer [OPTIONS]
```

## Description

Debug helper: build a pointer from a file, parse one from disk or stdin, or just check whether some bytes are a valid pointer

## Options

### Flags

- `-f`, `--file` `<FILE>`
    Build a pointer from this file (read content, hash, encode)

- `-p`, `--pointer` `<POINTER>`
    Parse and display this existing pointer file

- `--stdin`
    Read a pointer from stdin (mutually exclusive with --pointer)

- `--check`
    Validity check mode: exit 0 if input parses, 1 if not, 2 if `--strict` and not byte-canonical

- `--strict`
    In `--check`, also reject non-canonical pointers

- `--no-strict`
    Explicitly disable strict mode (paired with `--strict`)

## Reporting bugs

This command is from the Rust implementation of git-lfs, not the original
Go implementation. Please report bugs to our [issue tracker](https://gitlab.com/rustutils/git-lfs/issues).
