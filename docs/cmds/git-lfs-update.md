# git-lfs-update

## Name

`git-lfs-update` — Update Git hooks

## Synopsis

```
git-lfs-update [OPTIONS]
```

## Description

Update Git hooks

Update the Git hooks used by Git LFS. Silently upgrades known hook contents. If you have your own custom hooks you may need to use one of the extended options below.

## Options

### Flags

- `-f`, `--force`
    Forcibly overwrite any existing hooks with git-lfs hooks.

    Use this option if `git lfs update` fails because of existing hooks but you don't care about their current contents.

- `-m`, `--manual`
    Print instructions for manually updating your hooks to include git-lfs functionality.

    Use this option if `git lfs update` fails because of existing hooks and you want to retain their functionality.

## Reporting bugs

This command is from the Rust implementation of git-lfs, not the original
Go implementation. Please report bugs to our [issue tracker](https://gitlab.com/rustutils/git-lfs/issues).
