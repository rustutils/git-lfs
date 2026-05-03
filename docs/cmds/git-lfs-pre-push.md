# git-lfs-pre-push

## Name

`git-lfs-pre-push` — Git pre-push hook implementation

## Synopsis

```
git-lfs-pre-push [OPTIONS] <REMOTE> [URL]
```

## Description

Git pre-push hook implementation

Respond to Git pre-push events. Reads the range of commits from stdin in the form `<local-ref> <local-sha1> <remote-ref> <remote-sha1>`, takes the remote name and URL as arguments, and uploads any Git LFS objects associated with those commits to the Git LFS API.

When pushing a new branch, the list of Git objects considered is every object reachable from the new branch. When deleting a branch, no LFS objects are pushed.

## Options

### Arguments

- `<REMOTE>`
    Name of the remote being pushed to

- `<URL>`
    URL of the remote (informational; we use the `lfs.url` config)

### Flags

- `-d`, `--dry-run`
    Print the files that would be pushed, without actually pushing them

## See also

[git-lfs-clean(1)](./git-lfs-clean.md), [git-lfs-push(1)](./git-lfs-push.md).

## Reporting bugs

This command is from the Rust implementation of git-lfs, not the original
Go implementation. Please report bugs to our [issue tracker](https://gitlab.com/rustutils/git-lfs/issues).
