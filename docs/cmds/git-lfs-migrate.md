# git-lfs-migrate

## Name

`git-lfs-migrate` — Migrate history to or from Git LFS

## Synopsis

```
git-lfs-migrate <COMMAND>
```

## Description

Migrate history to or from Git LFS

Convert files in a Git repository to or from Git LFS pointers, or summarize Git file sizes by file type. The `import` mode converts Git files (i.e. blobs) to Git LFS, the `export` mode does the reverse, and the `info` mode provides an informational summary useful for deciding which files to import or export.

In all modes, by default `git lfs migrate` operates only on the currently checked-out branch, and only on files added in commits which do not exist on any remote. Multiple options are available to override these defaults — see INCLUDE AND EXCLUDE REFERENCES.

When converting files to or from Git LFS, this command only changes your local repository and working copy, never any remotes. `import` and `export` are generally DESTRUCTIVE — they rewrite Git history, changing commits and generating new commit SHAs. (The exception is the `--no-rewrite` `import` sub-mode.) Always commit or stash any uncommitted work first, validate the result before pushing, and force-push the new history once you're satisfied.

For `info` and `import`, all file types are considered by default. In `import` you'll usually want filename patterns or `--fixup`; `export` requires at least one `--include` pattern. See INCLUDE AND EXCLUDE.

`git lfs migrate` will examine, create, and modify `.gitattributes` files as necessary. They are always assigned the default read/write permissions mode; symbolic links with that name halt the migration.

## Options

### Subcommands

- `import` — Convert Git objects to Git LFS pointers
- `export` — Convert Git LFS pointers to Git objects
- `info` — Show information about repository size

## Include and exclude

You can have `git lfs migrate` convert only files whose pathspec
matches the `--include` glob patterns and does not match the
`--exclude` glob patterns, either to reduce total migration time or
to migrate part of your repo. Multiple patterns may be given using
commas as delimiters.

Pattern matching is functionally equivalent to the
`.gitattributes` format. In addition to simple file extension
matches (e.g. `*.gif`), patterns may also specify directory paths,
in which case the `path/**` form may be used to match recursively.

Note that this form of pattern matching for `--include` /
`--exclude` is unique to `git lfs migrate`. Other commands which
also take these options (such as `git lfs ls-files`) use the
[gitignore(5)](https://git-scm.com/docs/gitignore) form of pattern matching instead.

## Include and exclude references

You can have `git lfs migrate` convert only files added in commits
reachable from certain references — defined with `--include-ref` —
and ignore files in commits reachable from references defined with
`--exclude-ref`.

For example, given:

        D---E---F
       /         \
      A---B------C    refs/heads/my-feature
       \          \
        \          refs/heads/main
         \
          refs/remotes/origin/main

The commits reachable by each ref:

    refs/heads/main:           C, B, A
    refs/heads/my-feature:     F, E, D, B, A
    refs/remotes/origin/main:  A

The following options would include commits F, E, D, C, and B but
exclude commit A:

    --include-ref=refs/heads/my-feature
    --include-ref=refs/heads/main
    --exclude-ref=refs/remotes/origin/main

The presence of `--everything` indicates that all commits reachable
from all local and remote references should be migrated. Note that
the remote refs themselves are never updated by the migration.

## Examples

List the file types taking up the most space in your repository's
unpushed commits:

    git lfs migrate info

Convert specific file types in unpushed commits to LFS:

    git lfs migrate import --include="*.mp3,*.psd"

Check for large files and existing LFS objects across every branch:

    git lfs migrate info --everything

Convert all zip files in every local branch to LFS:

    git lfs migrate import --everything --include="*.zip"

Convert all files over 100K in every local branch:

    git lfs migrate import --everything --above=100Kb

Migrate to Git LFS in a single new commit (no history rewrite):

    git lfs track "*.zip" "*.mp3" "*.psd"
    git add .gitattributes
    git commit -m "add Git LFS attributes"
    git lfs migrate import --no-rewrite --yes test.zip audios/*.mp3 images/*.psd

Convert all zip Git LFS objects back to regular Git blobs:

    git lfs migrate export --include-ref=main --include="*.zip"

After any history-rewriting migration, force-push the rewritten
branches to your remotes — this alters Git history on your remotes
and should be done with care.

## See also

[git-lfs-checkout(1)](./git-lfs-checkout.md), [git-lfs-ls-files(1)](./git-lfs-ls-files.md), [git-lfs-track(1)](./git-lfs-track.md), [git-lfs-untrack(1)](./git-lfs-untrack.md), [gitattributes(5)](https://git-scm.com/docs/gitattributes), [gitignore(5)](https://git-scm.com/docs/gitignore).

## Reporting bugs

This command is from the Rust implementation of git-lfs, not the original
Go implementation. Please report bugs to our [issue tracker](https://gitlab.com/rustutils/git-lfs/issues).
