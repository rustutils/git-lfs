# git-lfs-install

## Name

`git-lfs-install` — Install Git LFS configuration

## Synopsis

```
git-lfs-install [OPTIONS]
```

## Description

Install Git LFS configuration

Set up the `lfs` smudge and clean filters under the name `lfs` in the global Git config, and (when run from inside a repository) install a pre-push hook to run [git-lfs-pre-push(1)](./git-lfs-pre-push.md). If `core.hooksPath` is configured in any Git configuration (supported on Git v2.9.0 or later), the pre-push hook is installed to that directory instead.

Without any options, only sets up the `lfs` smudge and clean filters if they are not already set.

## Options

### Flags

- `-f`, `--force`
    Set the `lfs` smudge and clean filters, overwriting existing values

- `-l`, `--local`
    Set the `lfs` smudge and clean filters in the local repository's git config, instead of the global git config (`~/.gitconfig`)

- `-w`, `--worktree`
    Set the `lfs` smudge and clean filters in the current working tree's git config, instead of the global git config (`~/.gitconfig`) or local repository's git config (`$GIT_DIR/config`).

    If multiple working trees are in use, the Git config extension `worktreeConfig` must be enabled to use this option. If only one working tree is in use, `--worktree` has the same effect as `--local`. Available only on Git v2.20.0 or later.

- `--system`
    Set the `lfs` smudge and clean filters in the system git config, e.g. `/etc/gitconfig` instead of the global git config (`~/.gitconfig`)

- `--file` `<PATH>`
    Set the `lfs` smudge and clean filters in the Git configuration file specified by `<PATH>`

- `-s`, `--skip-smudge`
    Skip automatic downloading of objects on clone or pull.

    Requires a manual `git lfs pull` every time a new commit is checked out on the repository.

- `--skip-repo`
    Skip installation of hooks into the local repository.

    Use if you want to install the LFS filters but not make changes to the hooks. Valid alongside `--local`, `--worktree`, `--system`, or `--file`.

## See also

[git-lfs-uninstall(1)](./git-lfs-uninstall.md), [git-worktree(1)](https://git-scm.com/docs/git-worktree).

## Reporting bugs

This command is from the Rust implementation of git-lfs, not the original
Go implementation. Please report bugs to our [issue tracker](https://gitlab.com/rustutils/git-lfs/issues).
