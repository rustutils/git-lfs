# git-lfs-uninstall

## Name

`git-lfs-uninstall` — Reverse of `install`: clear the `filter.lfs.*` config and remove the LFS git hooks. Hooks that don't match what we'd write are left untouched

## Synopsis

```
git-lfs-uninstall [OPTIONS] [MODE]
```

## Description

Reverse of `install`: clear the `filter.lfs.*` config and remove the LFS git hooks. Hooks that don't match what we'd write are left untouched

## Options

### Arguments

- `<MODE>`
    Optional mode: `hooks` removes only the LFS git hooks and leaves the filter config alone (the inverse of `--skip-repo`)

### Flags

- `-l`, `--local`
    Operate on the local repo only (default: --global)

- `--system`
    Operate on `/etc/gitconfig` (`git config --system`)

- `--worktree`
    Operate on `.git/config.worktree` for the current worktree

- `--file` `<PATH>`
    Operate on the given config file directly. Treated as "global-like" for the success message

- `--skip-repo`
    Only unset config; don't touch hooks

## Reporting bugs

This command is from the Rust implementation of git-lfs, not the original
Go implementation. Please report bugs to our [issue tracker](https://gitlab.com/rustutils/git-lfs/issues).
