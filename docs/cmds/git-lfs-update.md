# git-lfs-update

## Name

`git-lfs-update` — (Re-)install the four LFS git hooks (`pre-push`, `post-checkout`, `post-commit`, `post-merge`) for the current repository

## Synopsis

```
git-lfs-update [OPTIONS]
```

## Description

(Re-)install the four LFS git hooks (`pre-push`, `post-checkout`, `post-commit`, `post-merge`) for the current repository

## Options

### Flags

- `--force`
    Overwrite any custom hook contents

- `--manual`
    Print install instructions instead of writing the hook files

## Reporting bugs

This command is from the Rust implementation of git-lfs, not the original
Go implementation. Please report bugs to our [issue tracker](https://gitlab.com/rustutils/git-lfs/issues).
