# git-lfs-pre-push

## Name

`git-lfs-pre-push` — Git pre-push hook entry point — not typically invoked by hand. Reads `<local-ref> <local-sha> <remote-ref> <remote-sha>` lines from stdin and uploads the LFS objects newly reachable from each `<local-sha>`

## Synopsis

```
git-lfs-pre-push [OPTIONS] <REMOTE> [URL]
```

## Description

Git pre-push hook entry point — not typically invoked by hand. Reads `<local-ref> <local-sha> <remote-ref> <remote-sha>` lines from stdin and uploads the LFS objects newly reachable from each `<local-sha>`

## Options

### Arguments

- `<REMOTE>`
    Name of the remote being pushed to

- `<URL>`
    URL of the remote (informational; we use `lfs.url` config)

### Flags

- `--dry-run`
    List the objects that would be pushed without actually uploading them

## Reporting bugs

This command is from the Rust implementation of git-lfs, not the original
Go implementation. Please report bugs to our [issue tracker](https://gitlab.com/rustutils/git-lfs/issues).
