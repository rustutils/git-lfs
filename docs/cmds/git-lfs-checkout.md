# git-lfs-checkout

## Name

`git-lfs-checkout` — Populate working copy with real content from Git LFS files

## Synopsis

```
git-lfs-checkout [OPTIONS] [PATHS]...
```

## Description

Populate working copy with real content from Git LFS files.

Try to ensure that the working copy contains file content for Git LFS objects for the current ref, if the object data is available. Does not download any content; see [git-lfs-fetch(1)](./git-lfs-fetch.md) for that.

Checkout scans the current ref for all LFS objects that would be required, then where a file is either missing in the working copy, or contains placeholder pointer content with the same SHA, the real file content is written, provided we have it in the local store. Modified files are never overwritten.

One or more may be provided as arguments to restrict the set of files that are updated. Glob patterns are matched as per the format described in [gitignore(5)](https://git-scm.com/docs/gitignore).

When used with `--to` and the working tree is in a conflicted state due to a merge, this option checks out one of the three stages a conflicting Git LFS object into a separate file (which can be outside of the work tree). This can make using diff tools to inspect and resolve merges easier. A single Git LFS object's file path must be provided in `PATHS`. If `FILE` already exists, whether as a regular file, symbolic link, or directory, it will be removed and replaced, unless it is a non-empty directory or otherwise cannot be deleted.

If the installed Git version is at least 2.42.0, this command will by default check out Git LFS objects for files only if they are present in the Git index and if they match a Git LFS filter attribute from a `.gitattributes` file that is present in either the index or the current working tree (or, as is always the case, if they match a Git LFS filter attribute in a local gitattributes file such as `$GIT_DIR/info/attributes`). These constraints do not apply with prior versions of Git.

In a repository with a partial clone or sparse checkout, it is therefore advisable to check out all `.gitattributes` files from HEAD before using this command, if Git v2.42.0 or later is installed. Alternatively, the `GIT_ATTR_SOURCE` environment variable may be set to HEAD, which will cause Git to only read attributes from `.gitattributes` files in HEAD and ignore those in the index or working tree.

In a bare repository, this command prints an informational message and exits without modifying anything. In a future version, it may exit with an error.

## Options

### Arguments

- `<PATHS>`
    Paths to check out.

    When empty, everything in HEAD's tree is checked out. In conflict mode (`--to <path>` together with one of `--base`, `--ours`, or `--theirs`), exactly one path is required.

### Flags

- `--base`
    Check out the merge base of the specified file

- `--ours`
    Check out our side (that of the current branch) of the conflict for the specified file

- `--theirs`
    Check out their side (that of the other branch) of the conflict for the specified file

- `--to` `<FILE>`
    If the working tree is in a conflicted state, check out the portion of the conflict specified by `--base`, `--ours`, or `--theirs` to the given path. Exactly one of these options is required

## Examples

Checkout all files that are missing or placeholders:

    git lfs checkout

Checkout a specific couple of files:

    git lfs checkout path/to/file1.png path/to/file2.png

Checkout a path with a merge conflict into separate files:

    # Attempt merge with a branch that has a merge conflict
    $ git merge conflicting-branch
    CONFLICT (content): Merge conflict in path/to/conflicting/file.dat

    # Checkout versions of the conflicting file into temp files
    $ git lfs checkout --to ours.dat --ours path/to/conflicting/file.dat
    $ git lfs checkout --to theirs.dat --theirs path/to/conflicting/file.dat

    # Compare conflicting versions in ours.dat and theirs.dat,
    # then resolve conflict (e.g., by choosing one version over
    # the other, or creating a new version)

    # Cleanup and continue with merge
    $ rm ours.dat theirs.dat
    $ git add path/to/conflicting/file.dat
    $ git merge --continue

