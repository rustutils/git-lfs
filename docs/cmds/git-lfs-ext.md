# git-lfs-ext

## Name

`git-lfs-ext` — List the configured LFS pointer extensions

## Synopsis

```
git-lfs-ext [COMMAND]
```

## Description

List the configured LFS pointer extensions

Print each `lfs.extension.<name>.*` entry resolved to its final configuration in priority order. Extensions chain external clean / smudge programs around each LFS object — see [git-lfs-config(5)](./git-lfs-config.md) for how to configure them.

With no arguments, prints every configured extension. With `list <name>...`, prints only the named extensions (one block per name, in argument order).

## Options

### Subcommands

- `list` — List configured LFS pointer extensions, optionally filtered by name

## Examples

List details for all extensions:

    git lfs ext

or equivalently:

    git lfs ext list

List details for the specified extensions:

    git lfs ext list foo bar

## Reporting bugs

This command is from the Rust implementation of git-lfs, not the original
Go implementation. Please report bugs to our [issue tracker](https://gitlab.com/rustutils/git-lfs/issues).
