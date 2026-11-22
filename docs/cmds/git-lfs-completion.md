# git-lfs-completion

## Name

`git-lfs-completion` — Print a shell completion script

## Synopsis

```
git-lfs-completion [SHELL]
```

## Description

Print a shell completion script

Print a script that completes Git LFS commands and options in the given shell, for both `git lfs` and `git-lfs`. Load it from your shell's startup file, for example with `source <(git lfs completion bash)` in `~/.bashrc`, or save it where your shell looks for completions.

## Options

### Arguments

- `<SHELL>`
    The shell to print the script for

## Reporting bugs

This command is from the Rust implementation of git-lfs, not the original
Go implementation. Please report bugs to our [issue tracker](https://gitlab.com/rustutils/git-lfs/issues).
