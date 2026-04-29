# git-lfs-uninstall

## Name

`git-lfs-uninstall` — Reverse of `install`: clear the `filter.lfs.*` config and remove the LFS git hooks. Hooks that don't match what we'd write are left untouched

## Synopsis

```
git-lfs-uninstall [OPTIONS]
```

## Description

Reverse of `install`: clear the `filter.lfs.*` config and remove the LFS git hooks. Hooks that don't match what we'd write are left untouched

## Options

### Flags

- `-l`, `--local`
    Operate on the local repo only (default: --global)

- `--skip-repo`
    Only unset config; don't touch hooks

