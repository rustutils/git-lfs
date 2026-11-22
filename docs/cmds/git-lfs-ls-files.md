# git-lfs-ls-files

## Name

`git-lfs-ls-files` — Show information about Git LFS files in the index and working tree

## Synopsis

```
git-lfs-ls-files [OPTIONS] [REF]...
```

## Description

Show information about Git LFS files in the index and working tree

Display paths of Git LFS files that are found in the tree at the given reference. If no reference is given, scan the currently checked-out branch. If two references are given, list the LFS files added or changed in the commits after the first reference, up to and including the second.

An asterisk (`*`) after the OID indicates a full object, a minus (`-`) indicates an LFS pointer.

## Options

### Arguments

- `<REFS>`
    Ref to list, and optionally a second ref to list the changes up to. Defaults to HEAD

### Flags

- `-l`, `--long`
    Show the entire 64-character OID, instead of just the first 10

- `-s`, `--size`
    Show the size of the LFS object in parentheses at the end of each line

- `-n`, `--name-only`
    Show only the LFS-tracked file names

- `-a`, `--all`
    Inspect the full history of the repository, not the current HEAD (or other provided reference).

    Includes previous versions of LFS objects that are no longer found in the current tree.

- `-d`, `--debug`
    Show as much information as possible about an LFS file.

    Intended for manual inspection; the exact format may change at any time.

- `--deleted`
    Include LFS pointers reachable from history but no longer present in the current tree

- `-I`, `--include` `<INCLUDE>`
    Only list paths matching these patterns (comma-delimited).

    `lfs.fetchinclude` isn't used.

- `-X`, `--exclude` `<EXCLUDE>`
    Leave out paths matching these patterns (comma-delimited).

    `lfs.fetchexclude` isn't used.

- `-j`, `--json`
    Write Git LFS file information as JSON to standard output if the command exits successfully.

    Intended for interoperation with external tools. If `--debug` is also provided, that option takes precedence. If any of `--long`, `--size`, or `--name-only` are provided, those options will have no effect.

## See also

[git-lfs-status(1)](./git-lfs-status.md), [git-lfs-config(5)](./git-lfs-config.md).

## Reporting bugs

This command is from the Rust implementation of git-lfs, not the original
Go implementation. Please report bugs to our [issue tracker](https://gitlab.com/rustutils/git-lfs/issues).
