# git-lfs-locks

## Name

`git-lfs-locks` — Lists currently locked files from the Git LFS server

## Synopsis

```
git-lfs-locks [OPTIONS]
```

## Description

Lists currently locked files from the Git LFS server

Lists current locks from the Git LFS server. Without filters, all locks visible to the configured remote are returned.

## Options

### Flags

- `-r`, `--remote` `<REMOTE>`
    Specify the Git LFS server to use. Ignored if the `lfs.url` config key is set

- `-i`, `--id` `<ID>`
    Specify a lock by its ID. Returns a single result

- `-p`, `--path` `<PATH>`
    Specify a lock by its path. Returns a single result

- `--local`
    List only our own locks which are cached locally. Skips a remote call.

    Useful when offline or to confirm what `git lfs lock` recorded locally. Combine with `--path` / `--id` / `--limit` to filter; `--verify` is rejected.

- `--verify`
    Verify the lock owner on the server and mark our own locks with `O`.

    Own locks are held by us and the corresponding files can be updated for the next push. All other locks are held by someone else. Contrary to `--local`, this also detects locks held by us despite no local lock information being available (e.g. because the file had been locked from a different clone) and detects "broken" locks (e.g. someone else forcibly unlocked our files).

- `-l`, `--limit` `<LIMIT>`
    Maximum number of results to return

- `-j`, `--json`
    Write lock info as JSON to standard output if the command exits successfully.

    Intended for interoperation with external tools. If the command returns with a non-zero exit code, plain text messages are sent to standard error.

- `--ref` `<REFSPEC>`
    Refspec to filter locks by (extension over upstream).

    Defaults to the current branch's tracked upstream — same auto-resolution as `git lfs lock`.

## See also

[git-lfs-lock(1)](./git-lfs-lock.md), [git-lfs-unlock(1)](./git-lfs-unlock.md).

## Reporting bugs

This command is from the Rust implementation of git-lfs, not the original
Go implementation. Please report bugs to our [issue tracker](https://gitlab.com/rustutils/git-lfs/issues).
