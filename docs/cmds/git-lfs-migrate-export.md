# git-lfs-migrate-export

## Name

`git-lfs-migrate-export` — Convert Git LFS pointers to Git objects

## Synopsis

```
git-lfs-migrate-export [OPTIONS] [BRANCHES]...
```

## Description

Convert Git LFS pointers to Git objects

Migrate Git LFS pointer files present in the Git history out of Git LFS, converting them back into their corresponding object files. Files matching the `--include` patterns are removed from Git LFS; files matching `--exclude` retain their LFS status. Modifies `.gitattributes` to set/unset the relevant filepath patterns.

At least one `--include` pattern is required. Objects not present in the local LFS store are downloaded from the `--remote` (defaults to `origin`). Pointers whose objects can't be fetched are left as-is.

## Options

### Arguments

- `<BRANCHES>`
    Branches to rewrite (default: the currently checked-out branch). References prefixed with `^` are excluded

### Flags

- `-I`, `--include` `<INCLUDE>`
    Convert pointers at paths matching this glob (repeatable, comma-delimited). Required — at least one must be given

- `-X`, `--exclude` `<EXCLUDE>`
    Don't convert pointers at paths matching this glob (repeatable, comma-delimited)

- `--include-ref` `<INCLUDE_REF>`
    Restrict the rewrite to commits reachable from these refs. Repeatable

- `--exclude-ref` `<EXCLUDE_REF>`
    Exclude commits reachable from these refs. Repeatable

- `--everything`
    Consider all commits reachable from any local or remote ref.

    Only local refs are updated even with `--everything`; remote refs stay synchronized with their remote.

- `--object-map` `<OBJECT_MAP>`
    Write a CSV of `<OLD-SHA>,<NEW-SHA>` for every rewritten commit to the named file.

    Useful as input to `git filter-repo` or other downstream tools.

- `--verbose`
    Print the commit OID and filename of migrated files to standard output

- `--remote` `<REMOTE>`
    Download LFS objects from this remote during the export. Defaults to `origin`

- `--skip-fetch`
    Don't refresh the known set of remote references before the rewrite

- `--yes`
    Assume a yes answer to any prompts, permitting noninteractive use.

    Currently we don't prompt for any reason, so this is accepted as a no-op for upstream parity.

## Examples

Convert all zip Git LFS pointers on `main` back to regular Git blobs:

    git lfs migrate export --include-ref=main --include="*.zip"

Pointers whose objects aren't in the local store are downloaded from the `--remote` (defaults to `origin`); pointers that can't be downloaded are left as-is.

After exporting, the rewritten branches need to be force-pushed — this rewrites history on the remote.

## See also

[git-lfs-migrate(1)](./git-lfs-migrate.md), [git-lfs-migrate-import(1)](./git-lfs-migrate-import.md), [git-lfs-migrate-info(1)](./git-lfs-migrate-info.md).

## Reporting bugs

This command is from the Rust implementation of git-lfs, not the original
Go implementation. Please report bugs to our [issue tracker](https://gitlab.com/rustutils/git-lfs/issues).
