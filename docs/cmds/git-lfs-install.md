# git-lfs-install

## Name

`git-lfs-install` — Configure git to invoke git-lfs as the clean/smudge/process filter, and install the LFS git hooks

## Synopsis

```
git-lfs-install [OPTIONS]
```

## Description

Configure git to invoke git-lfs as the clean/smudge/process filter, and install the LFS git hooks

## Options

### Flags

- `-l`, `--local`
    Set config in the local repo only (default: --global)

- `--system`
    Operate on `/etc/gitconfig` (`git config --system`)

- `--worktree`
    Operate on `.git/config.worktree` for the current worktree

- `--file` `<PATH>`
    Operate on the given config file directly. Treated as "global-like" for the success message

- `-f`, `--force`
    Overwrite existing config and hooks

- `--skip-repo`
    Only set the filter config; don't install hooks

- `--skip-smudge`
    Configure the smudge filter to pass pointer text through unchanged. Use with a follow-up `git lfs pull` to download content on demand

