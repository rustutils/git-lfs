# git-lfs-migrate-import

## Name

`git-lfs-migrate-import` — Convert Git objects to Git LFS pointers

## Synopsis

```
git-lfs-migrate-import [OPTIONS] [ARGS]...
```

## Description

Convert Git objects to Git LFS pointers

Migrate objects present in the Git history to pointer files tracked and stored with Git LFS. Adds entries for the converted file types to `.gitattributes`, creating those files if they don't exist — as if `git lfs track` had been run at the points in history where each type first appears.

With `--fixup`, examine existing `.gitattributes` files and convert only Git objects that should be tracked by Git LFS according to those rules but aren't yet.

With `--no-rewrite`, migrate objects to pointers in a single new commit on top of HEAD without rewriting history. The base `migrate` options (`--include-ref`, `--everything`, etc.) are ignored in this sub-mode, and the positional argument list changes from branches to a list of files. Files must be tracked by patterns already in `.gitattributes`.

## Options

### Arguments

- `<ARGS>`
    Branches to rewrite (default: the currently checked-out branch). With `--no-rewrite`, instead a list of working-tree files to convert. References prefixed with `^` are excluded

### Flags

- `-I`, `--include` `<INCLUDE>`
    Convert paths matching this glob (repeatable, comma-delimited). Required unless `--above` is set or `--no-rewrite` is given

- `-X`, `--exclude` `<EXCLUDE>`
    Exclude paths matching this glob (repeatable, comma-delimited)

- `--include-ref` `<INCLUDE_REF>`
    Restrict the rewrite to commits reachable from these refs. Repeatable

- `--exclude-ref` `<EXCLUDE_REF>`
    Exclude commits reachable from these refs. Repeatable

- `--everything`
    Consider all commits reachable from any local or remote ref.

    Only local refs are updated even with `--everything`; remote refs stay synchronized with their remote.

- `--above` `<ABOVE>`
    Only migrate files whose individual filesize is above the given size (e.g. `1b`, `20 MB`, `3 TiB`).

    Cannot be used with `--include`, `--exclude`, or `--fixup`.

- `--no-rewrite`
    Migrate objects in a new commit on top of HEAD without rewriting Git history.

    Switches to a different argument list (positional args become files, not branches) and ignores the core `migrate` options (`--include-ref`, `--everything`, etc.).

- `-m`, `--message` `<MESSAGE>`
    Commit message for the `--no-rewrite` commit.

    If omitted, a message is generated from the file arguments.

- `--fixup`
    Infer `--include` and `--exclude` filters per-commit from the repository's `.gitattributes` files.

    Imports filepaths that should be tracked by Git LFS but aren't yet pointers. Incompatible with explicitly given `--include` / `--exclude` filters.

- `--object-map` `<OBJECT_MAP>`
    Write a CSV of `<OLD-SHA>,<NEW-SHA>` for every rewritten commit to the named file

- `--verbose`
    Print the commit OID and filename of migrated files to standard output

- `--remote` `<REMOTE>`
    Remote to consult when fetching missing LFS objects (default `origin`)

- `--skip-fetch`
    Don't refresh the known set of remote references before determining the set of "un-pushed" commits to migrate.

    Has no effect when combined with `--include-ref` or `--exclude-ref`.

- `--yes`
    Assume a yes answer to any prompts, permitting noninteractive use.

    Currently we don't prompt for any reason, so this is accepted as a no-op for upstream parity.

## Reporting bugs

This command is from the Rust implementation of git-lfs, not the original
Go implementation. Please report bugs to our [issue tracker](https://gitlab.com/rustutils/git-lfs/issues).
