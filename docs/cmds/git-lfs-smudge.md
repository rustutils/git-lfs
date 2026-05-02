# git-lfs-smudge

## Name

`git-lfs-smudge` — Git smudge filter that converts pointer in blobs to the actual content

## Synopsis

```
git-lfs-smudge [OPTIONS] [PATH]
```

## Description

Git smudge filter that converts pointer in blobs to the actual content

Read a Git LFS pointer file from standard input and write the contents of the corresponding large file to standard output. If needed, download the file’s contents from the Git LFS endpoint. The argument, if provided, is only used for a progress bar.

Smudge is typically run by Git’s smudge filter, configured by the repository’s Git attributes.

In your Git configuration or in a .lfsconfig file, you may set either or both of `lfs.fetchinclude` and `lfs.fetchexclude` to comma-separated lists of paths. If `lfs.fetchinclude` is defined, Git LFS pointer files will only be replaced with the contents of the corresponding Git LFS object file if their path matches one in that list, and if `lfs.fetchexclude` is defined, Git LFS pointer files will only be replaced with the contents of the corresponding Git LFS object file if their path does not match one in that list. Paths are matched using wildcard matching as per [gitignore(5)](https://git-scm.com/docs/gitignore). Git LFS pointer files that are not replaced with the contents of their corresponding object files are simply copied to standard output without change.

Without any options, git lfs smudge outputs the raw Git LFS content to standard output.

## Options

### Arguments

- `<PATH>`
    Working-tree path of the file being smudged (currently unused)

### Flags

- `--skip`
    Skip automatic downloading of objects on clone or pull.

    Equivalent to `GIT_LFS_SKIP_SMUDGE=1`. Wired up by `git lfs install --skip-smudge`.

## Environment

`GIT_LFS_SKIP_SMUDGE`
  : Disables the smudging process. For more information, see: [git-lfs-config(5)](./git-lfs-config.md)

## Known bugs

On Windows, Git before 2.34.0 does not handle files in the working tree
larger than 4 gigabytes. Newer versions of Git, as well as Unix
versions, are unaffected.

## Reporting bugs

This command is from the Rust implementation of git-lfs, not the original
Go implementation. Please report bugs to our [issue tracker](https://gitlab.com/rustutils/git-lfs/issues).
