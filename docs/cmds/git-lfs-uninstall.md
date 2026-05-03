# git-lfs-uninstall

## Name

`git-lfs-uninstall` — Remove Git LFS configuration

## Synopsis

```
git-lfs-uninstall [OPTIONS] [MODE]
```

## Description

Remove Git LFS configuration

Remove the `lfs` clean and smudge filters from the global Git config, and (when run from inside a Git repository) uninstall the Git LFS pre-push hook. Hooks that don't match what we would write are left untouched.

## Options

### Arguments

- `<MODE>`
    Optional mode. With `hooks`, removes only the LFS git hooks and leaves the filter config alone (the inverse of `--skip-repo`)

### Flags

- `-l`, `--local`
    Remove the `lfs` smudge and clean filters from the local repository's git config, instead of the global git config (`~/.gitconfig`)

- `-w`, `--worktree`
    Remove the `lfs` smudge and clean filters from the current working tree's git config, instead of the global git config (`~/.gitconfig`) or local repository's git config (`$GIT_DIR/config`).

    If multiple working trees are in use, the Git config extension `worktreeConfig` must be enabled to use this option. If only one working tree is in use, `--worktree` has the same effect as `--local`. Available only on Git v2.20.0 or later.

- `--system`
    Remove the `lfs` smudge and clean filters from the system git config, instead of the global git config (`~/.gitconfig`)

- `--file` `<PATH>`
    Remove the `lfs` smudge and clean filters from the Git configuration file specified by `<PATH>`

- `--skip-repo`
    Skip cleanup of the local repo.

    Use if you want to uninstall the global LFS filters but not make changes to the current repo.

## See also

[git-lfs-install(1)](./git-lfs-install.md), [git-worktree(1)](https://git-scm.com/docs/git-worktree).

## Reporting bugs

This command is from the Rust implementation of git-lfs, not the original
Go implementation. Please report bugs to our [issue tracker](https://gitlab.com/rustutils/git-lfs/issues).
