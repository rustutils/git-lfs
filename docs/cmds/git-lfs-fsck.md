# git-lfs-fsck

## Name

`git-lfs-fsck` — Check Git LFS files for consistency

## Synopsis

```
git-lfs-fsck [OPTIONS] [REFSPEC]
```

## Description

Check Git LFS files for consistency

Check all Git LFS files in the current HEAD for consistency. Corrupted files are moved to `.git/lfs/bad`.

A single committish may be given to inspect that commit instead of HEAD. The `<a>..<b>` range form from upstream is not yet supported — only a single ref is accepted. With no argument, HEAD is examined.

The default is to perform all checks. `lfs.fetchexclude` is also not yet honored on this command; objects whose paths match the exclude list will still be checked.

## Options

### Arguments

- `<REFSPEC>`
    Ref to scan. Defaults to HEAD

### Flags

- `--objects`
    Check that each object in HEAD matches its expected hash and that each object exists on disk

- `--pointers`
    Check that each pointer is canonical and that each file which should be stored as a Git LFS file is so stored

- `-d`, `--dry-run`
    Perform checks, but do not move any corrupted files to `.git/lfs/bad`

## See also

[git-lfs-ls-files(1)](./git-lfs-ls-files.md), [git-lfs-status(1)](./git-lfs-status.md), [gitignore(5)](https://git-scm.com/docs/gitignore).

## Reporting bugs

This command is from the Rust implementation of git-lfs, not the original
Go implementation. Please report bugs to our [issue tracker](https://gitlab.com/rustutils/git-lfs/issues).
