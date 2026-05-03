# git-lfs-post-commit

## Name

`git-lfs-post-commit` — Git post-commit hook implementation

## Synopsis

```
git-lfs-post-commit [ARGS]...
```

## Description

Git post-commit hook implementation

Respond to Git post-commit events. Like `git lfs post-merge`, we make sure that any files which are marked as lockable by `git lfs track` are read-only in the working copy, if not currently locked by the local user.

Upstream optimizes by only checking files changed in HEAD; we currently scan the full work tree on every commit. The result is the same, but slower on large repositories.

## Options

### Arguments

- `<ARGS>`
    Positional arguments passed by git. Not normally invoked by hand

## See also

[git-lfs-post-merge(1)](./git-lfs-post-merge.md), [git-lfs-track(1)](./git-lfs-track.md).

## Reporting bugs

This command is from the Rust implementation of git-lfs, not the original
Go implementation. Please report bugs to our [issue tracker](https://gitlab.com/rustutils/git-lfs/issues).
