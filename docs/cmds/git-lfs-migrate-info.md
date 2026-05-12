# git-lfs-migrate-info

## Name

`git-lfs-migrate-info` — Show information about repository size

## Synopsis

```
git-lfs-migrate-info [OPTIONS] [BRANCHES]...
```

## Description

Show information about repository size

Summarize the sizes of file objects present in the Git history, grouped by filename extension. Read-only — no objects or history change.

Existing Git LFS pointers are followed by default (the size of the referenced objects is totaled in a separate "LFS Objects" line). Use `--pointers=ignore` to skip pointers entirely, or `--pointers=no-follow` to count the pointer-text size as if the pointers were regular files (the older Git LFS behavior).

## Options

### Arguments

- `<BRANCHES>`
    Branches to scan (default: the currently checked-out branch). References prefixed with `^` are excluded

### Flags

- `-I`, `--include` `<INCLUDE>`
    Only include paths matching this glob (repeatable, comma-delimited)

- `-X`, `--exclude` `<EXCLUDE>`
    Exclude paths matching this glob (repeatable, comma-delimited)

- `--include-ref` `<INCLUDE_REF>`
    Restrict the scan to commits reachable from these refs. Repeatable

- `--exclude-ref` `<EXCLUDE_REF>`
    Exclude commits reachable from these refs. Repeatable

- `--everything`
    Consider all commits reachable from any local or remote ref

- `--above` `<ABOVE>`
    Only count files whose individual filesize is above the given size (e.g. `1b`, `20 MB`, `3 TiB`).

    File-extension groups whose largest file is below `--above` don't appear in the output.

- `--top` `<TOP>`
    Display the top N entries, ordered by total file count.

    Default 5. When existing Git LFS objects are found, an extra "LFS Objects" line is output in addition to the top N entries (unless `--pointers` changes this).

- `--pointers` `<POINTERS>`
    How to handle existing LFS pointer blobs.

    `follow` (default): summarize referenced objects in a separate "LFS Objects" line. `ignore`: skip pointers entirely. `no-follow`: count pointer-text size as if pointers were regular files (the older Git LFS behavior). When `--fixup` is given, defaults to `ignore`.

- `--unit` `<UNIT>`
    Format byte quantities in this unit.

    Valid units: `b, kib, mib, gib, tib, pib` (IEC) or `b, kb, mb, gb, tb, pb` (SI). Auto-scaled when omitted.

- `--fixup`
    Infer `--include` and `--exclude` filters per-commit from the repository's `.gitattributes` files.

    Counts filepaths that should be tracked by Git LFS but aren't yet pointers. Incompatible with explicit `--include` / `--exclude` filters and with `--pointers` settings other than `ignore`. Implies `--pointers=ignore` if not set.

- `--skip-fetch`
    Don't refresh the known set of remote references before the scan

- `--remote` `<REMOTE>`
    Remote to consult (currently a no-op; reserved for the auto-fetch path)

## Examples

List the file types taking up the most space in unpushed commits:

    git lfs migrate info

Check large files and existing LFS objects across every branch (local + remote):

    git lfs migrate info --everything

Report files that should be tracked by Git LFS according to the repository's `.gitattributes` but aren't yet pointers — the candidate set for `git lfs migrate import --fixup`:

    git lfs migrate info --fixup

## See also

[git-lfs-migrate(1)](./git-lfs-migrate.md), [git-lfs-migrate-import(1)](./git-lfs-migrate-import.md), [git-lfs-migrate-export(1)](./git-lfs-migrate-export.md).

## Reporting bugs

This command is from the Rust implementation of git-lfs, not the original
Go implementation. Please report bugs to our [issue tracker](https://gitlab.com/rustutils/git-lfs/issues).
