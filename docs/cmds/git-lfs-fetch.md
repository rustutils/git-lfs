# git-lfs-fetch

## Name

`git-lfs-fetch` — Download all Git LFS files for a given ref

## Synopsis

```
git-lfs-fetch [OPTIONS] [ARGS]...
```

## Description

Download all Git LFS files for a given ref

Download Git LFS objects at the given refs from the specified remote. See DEFAULT REMOTE and DEFAULT REFS for what happens if you don't specify.

This does not update the working copy; use [git-lfs-pull(1)](./git-lfs-pull.md) to download and replace pointer text with object content, or [git-lfs-checkout(1)](./git-lfs-checkout.md) to materialize already-downloaded objects.

## Options

### Arguments

- `<ARGS>`
    Optional remote name followed by refs. The first positional argument is treated as a remote name when it resolves; any following arguments are refs to fetch

### Flags

- `-I`, `--include` `<INCLUDE>`
    Specify `lfs.fetchinclude` just for this invocation; see INCLUDE AND EXCLUDE

- `-X`, `--exclude` `<EXCLUDE>`
    Specify `lfs.fetchexclude` just for this invocation; see INCLUDE AND EXCLUDE

- `-a`, `--all`
    Download all objects that are referenced by any commit reachable from the refs provided as arguments.

    If no refs are provided, then all refs are fetched. This is primarily for backup and migration purposes. Cannot be combined with `--include`/`--exclude`. Ignores any globally configured include and exclude paths to ensure that all objects are downloaded.

- `--stdin`
    Read a list of newline-delimited refs from standard input instead of the command line

- `-p`, `--prune`
    Prune old and unreferenced objects after fetching, equivalent to running `git lfs prune` afterwards. See [git-lfs-prune(1)](./git-lfs-prune.md) for more details

- `--refetch`
    Also fetch objects that are already present locally.

    Useful for recovery from a corrupt local store.

- `-d`, `--dry-run`
    Print what would be fetched, without actually fetching anything

- `-j`, `--json`
    Write the details of all object transfer requests as JSON to standard output.

    Intended for interoperation with external tools. When `--dry-run` is also specified, writes the details of the transfers that would occur if the objects were fetched.

## Default remote

Without arguments, fetch downloads from the default remote. The default
remote is the same as for `git fetch`, i.e. based on the remote branch
you're tracking first, or `origin` otherwise.

## Default refs

If no refs are given as arguments, the currently checked out ref is
used.

Note: upstream's `--recent` mode and the corresponding
`lfs.fetchrecent*` configuration aren't yet supported. The `--recent`
flag is omitted from this implementation; recently changed refs and
commits are not added to the fetch set.

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

Examples:

`git config lfs.fetchinclude "textures,images/foo*"`
:   This will only fetch objects referenced in paths in the `textures`
    folder, and files called `foo*` in the `images` folder.

`git config lfs.fetchinclude "*.jpg,*.png,*.tga"`
:   Only fetch JPG/PNG/TGA files, wherever they are in the repository.

`git config lfs.fetchexclude "media/reallybigfiles"`
:   Don't fetch any LFS objects referenced in the folder
    `media/reallybigfiles`, but fetch everything else.

`git config lfs.fetchinclude "media"`<br/>
`git config lfs.fetchexclude "media/excessive"`
:   Only fetch LFS objects in the `media` folder, but exclude those
    in one of its subfolders.

## Examples

Fetch the LFS objects for the current ref from the default remote:

    git lfs fetch

Fetch the LFS objects for the current ref from a secondary remote
`upstream`:

    git lfs fetch upstream

Fetch all the LFS objects from the default remote that are referenced
by any commit in the `main` and `develop` branches:

    git lfs fetch --all origin main develop

Fetch the LFS objects for a branch from `origin`:

    git lfs fetch origin mybranch

Fetch the LFS objects for two branches and a commit from `origin`:

    git lfs fetch origin main mybranch e445b45c1c9c6282614f201b62778e4c0688b5c8

## See also

[git-lfs-checkout(1)](./git-lfs-checkout.md), [git-lfs-pull(1)](./git-lfs-pull.md), [git-lfs-prune(1)](./git-lfs-prune.md), [gitconfig(5)](https://git-scm.com/docs/gitconfig).

