# git-lfs-logs

## Name

`git-lfs-logs` — Show errors logged by Git LFS

## Synopsis

```
git-lfs-logs [COMMAND]
```

## Description

Show errors logged by Git LFS

Manages the local log directory under `.git/lfs/logs`. Run with no subcommand to list log filenames; `last` prints the most recent log; `show <name>` prints a specific log; `clear` deletes them all. `boomtown` is a self-test that intentionally panics, writes a log file, and exits non-zero.

## Options

### Subcommands

- `last` — Print the most recent log to stdout
- `show` — Print the named log to stdout
- `clear` — Delete every log under `.git/lfs/logs`
- `boomtown` — Self-test: write a sample crash log and exit with status 2

## Reporting bugs

This command is from the Rust implementation of git-lfs, not the original
Go implementation. Please report bugs to our [issue tracker](https://gitlab.com/rustutils/git-lfs/issues).
