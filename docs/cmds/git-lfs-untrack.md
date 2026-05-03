# git-lfs-untrack

## Name

`git-lfs-untrack` — Remove Git LFS paths from Git attributes

## Synopsis

```
git-lfs-untrack [PATTERNS]...
```

## Description

Remove Git LFS paths from Git attributes

Stop tracking the given path(s) through Git LFS. The argument can be a glob pattern or a file path. The matching pointer files in history (and the objects in the local store) are left in place.

## Options

### Arguments

- `<PATTERNS>`
    Paths or glob patterns to stop tracking

## Examples

Configure Git LFS to stop tracking GIF files:

    git lfs untrack "*.gif"

## See also

[git-lfs-track(1)](./git-lfs-track.md), [git-lfs-install(1)](./git-lfs-install.md), [gitattributes(5)](https://git-scm.com/docs/gitattributes).

## Reporting bugs

This command is from the Rust implementation of git-lfs, not the original
Go implementation. Please report bugs to our [issue tracker](https://gitlab.com/rustutils/git-lfs/issues).
