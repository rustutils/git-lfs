# git-lfs-post-merge

## Name

`git-lfs-post-merge` — Git post-merge hook implementation

## Synopsis

```
git-lfs-post-merge [ARGS]...
```

## Description

Git post-merge hook implementation

Respond to Git post-merge events. Git invokes this hook with `<is-squash>`. We make sure that any files which are marked as lockable by `git lfs track` are read-only in the working copy, if not currently locked by the local user.

## Options

### Arguments

- `<ARGS>`
    Positional arguments passed by git. Not normally invoked by hand

## See also

[git-lfs-track(1)](./git-lfs-track.md).

## Reporting bugs

This command is from the Rust implementation of git-lfs, not the original
Go implementation. Please report bugs to our [issue tracker](https://gitlab.com/rustutils/git-lfs/issues).
