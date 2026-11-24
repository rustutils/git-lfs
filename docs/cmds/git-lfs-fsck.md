# git-lfs-fsck

## Name

`git-lfs-fsck` — Check Git LFS files for consistency

## Synopsis

```
git-lfs-fsck [OPTIONS] [REFSPEC]
```

## Description

Check Git LFS files for consistency

Check all Git LFS files in the current HEAD, and the objects of pointers staged in the index, for consistency. Corrupted files are moved to `.git/lfs/bad`.

A committish may be given to inspect that commit instead, or a range `<a>..<b>` to inspect the commits in it. The index isn't checked then.

The default is to perform all checks. Objects whose paths match `lfs.fetchexclude` aren't expected locally, so they aren't checked.

## Options

### Arguments

- `<REFSPEC>`
    Ref or range to scan. Defaults to HEAD and the index

### Flags

- `--objects`
    Check that each object matches its expected hash and exists on disk

- `--pointers`
    Check that each pointer is canonical and that each file which should be stored as a Git LFS file is so stored

- `-d`, `--dry-run`
    Perform checks, but do not move any corrupted files to `.git/lfs/bad`

## See also

[git-lfs-ls-files(1)](./git-lfs-ls-files.md), [git-lfs-status(1)](./git-lfs-status.md), [gitignore(5)](https://git-scm.com/docs/gitignore).

## Reporting bugs

This command is from the Rust implementation of git-lfs, not the original
Go implementation. Please report bugs to our [issue tracker](https://gitlab.com/rustutils/git-lfs/issues).
