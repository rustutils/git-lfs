# git-lfs-pull

## Name

`git-lfs-pull` — Download all Git LFS files for current ref and checkout

## Synopsis

```
git-lfs-pull [OPTIONS] [ARGS]...
```

## Description

Download all Git LFS files for current ref and checkout

Download Git LFS objects for the currently checked out ref, and update the working copy with the downloaded content if required.

This is generally equivalent to running `git lfs fetch [options] [<remote>]` followed by `git lfs checkout`. See [git-lfs-checkout(1)](./git-lfs-checkout.md) for partial-clone, sparse-checkout, and bare-repository behavior (governed by the installed Git version and `GIT_ATTR_SOURCE`).

Requires `git lfs install` to have wired up the smudge filter. If the filter is missing, the fetch step still runs but the working-tree update is skipped with a hint to install.

## Options

### Arguments

- `<ARGS>`
    Optional remote name followed by refs.

    The first positional argument is treated as a remote name when it resolves; any following arguments are refs to fetch. With no arguments, the default remote is used.

### Flags

- `-I`, `--include` `<INCLUDE>`
    Specify `lfs.fetchinclude` just for this invocation

- `-X`, `--exclude` `<EXCLUDE>`
    Specify `lfs.fetchexclude` just for this invocation

## Default remote

Without arguments, pull downloads from the default remote. The default
remote is the same as for `git pull`, i.e. based on the remote branch
you're tracking first, or `origin` otherwise.

## Include and exclude

You can configure Git LFS to only fetch objects to satisfy references
in certain paths of the repo, and/or to exclude certain paths of the
repo, to reduce the time you spend downloading things you do not use.

In your Git configuration or in a `.lfsconfig` file, you may set
either or both of `lfs.fetchinclude` and `lfs.fetchexclude` to
comma-separated lists of paths. If `lfs.fetchinclude` is defined, Git
LFS objects will only be fetched if their path matches one in that
list, and if `lfs.fetchexclude` is defined, Git LFS objects will only
be fetched if their path does not match one in that list. Paths are
matched using wildcard matching as per [gitignore(5)](https://git-scm.com/docs/gitignore).

Note that using the command-line options `-I` and `-X` override the
respective configuration settings. Setting either option to an empty
string clears the value.

## See also

[git-lfs-fetch(1)](./git-lfs-fetch.md), [git-lfs-checkout(1)](./git-lfs-checkout.md), [gitattributes(5)](https://git-scm.com/docs/gitattributes), [gitignore(5)](https://git-scm.com/docs/gitignore).

## Reporting bugs

This command is from the Rust implementation of git-lfs, not the original
Go implementation. Please report bugs to our [issue tracker](https://gitlab.com/rustutils/git-lfs/issues).
