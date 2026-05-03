# git-lfs-status

## Name

`git-lfs-status` — Show the status of Git LFS files in the working tree

## Synopsis

```
git-lfs-status [OPTIONS]
```

## Description

Show the status of Git LFS files in the working tree

Display paths of Git LFS objects that have not been pushed to the Git LFS server (large files that would be uploaded by `git push`), that have differences between the index file and the current HEAD commit (large files that would be committed by `git commit`), or that have differences between the working tree and the index file (files that could be staged with `git add`).

Must be run in a non-bare repository.

## Options

### Flags

- `-p`, `--porcelain`
    Give the output in an easy-to-parse format for scripts

- `-j`, `--json`
    Write Git LFS file status information as JSON to standard output if the command exits successfully.

    Intended for interoperation with external tools. If `--porcelain` is also provided, that option takes precedence.

## See also

[git-lfs-ls-files(1)](./git-lfs-ls-files.md).

## Reporting bugs

This command is from the Rust implementation of git-lfs, not the original
Go implementation. Please report bugs to our [issue tracker](https://gitlab.com/rustutils/git-lfs/issues).
