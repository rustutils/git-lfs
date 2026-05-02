# git-lfs-push

## Name

`git-lfs-push` — Push queued large files to the Git LFS endpoint

## Synopsis

```
git-lfs-push [OPTIONS] <REMOTE> [ARGS]...
```

## Description

Push queued large files to the Git LFS endpoint

Upload Git LFS files to the configured endpoint for the current Git remote. By default, filters out objects that are already referenced by the local clone of the remote (approximated via `refs/remotes/<remote>/*`); the server's batch API dedupes again, so a missing local tracking ref doesn't waste bandwidth.

## Options

### Arguments

- `<REMOTE>`
    Remote to push to (e.g. `origin`). The remote's tracking refs are excluded from the upload set so already-pushed objects aren't sent again

- `<ARGS>`
    Refs (or, with `--object-id`, raw OIDs) to push. With `--all`, restricts the all-refs walk to these; with `--stdin`, ignored (a warning is emitted)

### Flags

- `-d`, `--dry-run`
    Print the files that would be pushed, without actually pushing them

- `-a`, `--all`
    Push all objects reachable from the refs given as arguments.

    If no refs are provided, all local refs are pushed. Note this behavior differs from `git lfs fetch --all`, which fetches every ref including refs outside `refs/heads` / `refs/tags`. If you're migrating a repository, run `git lfs push` for any additional remote refs that contain LFS objects not reachable from your local refs.

- `-o`, `--object-id`
    Push only the object OIDs listed on the command line (or read from stdin with `--stdin`), separated by spaces

- `--stdin`
    Read newline-delimited refs (or object IDs when using `--object-id`) from standard input instead of the command line

## See also

[git-lfs-fetch(1)](./git-lfs-fetch.md), [git-lfs-pre-push(1)](./git-lfs-pre-push.md).

## Reporting bugs

This command is from the Rust implementation of git-lfs, not the original
Go implementation. Please report bugs to our [issue tracker](https://gitlab.com/rustutils/git-lfs/issues).
