# git-lfs-untrack

## Name

`git-lfs-untrack` — Stop tracking a file pattern with git-lfs by removing it from .gitattributes. The matching pointer files in history (and the objects in the local store) are left in place

## Synopsis

```
git-lfs-untrack [PATTERNS]...
```

## Description

Stop tracking a file pattern with git-lfs by removing it from .gitattributes. The matching pointer files in history (and the objects in the local store) are left in place

## Options

### Arguments

- `<PATTERNS>`
    File patterns to untrack

## Reporting bugs

This command is from the Rust implementation of git-lfs, not the original
Go implementation. Please report bugs to our [issue tracker](https://gitlab.com/rustutils/git-lfs/issues).
