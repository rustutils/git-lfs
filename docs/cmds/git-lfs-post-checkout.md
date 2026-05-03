# git-lfs-post-checkout

## Name

`git-lfs-post-checkout` — Git post-checkout hook implementation

## Synopsis

```
git-lfs-post-checkout [ARGS]...
```

## Description

Git post-checkout hook implementation

Respond to Git post-checkout events. Git invokes this hook with `<rev-before> <ref-after> <is-branch-checkout>`. We make sure that any files which are marked as lockable by `git lfs track` are read-only in the working copy, if not currently locked by the local user.

## Options

### Arguments

- `<ARGS>`
    Positional arguments passed by git. Not normally invoked by hand

## See also

[git-lfs-track(1)](./git-lfs-track.md).

## Reporting bugs

This command is from the Rust implementation of git-lfs, not the original
Go implementation. Please report bugs to our [issue tracker](https://gitlab.com/rustutils/git-lfs/issues).
