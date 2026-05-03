# git-lfs-unlock

## Name

`git-lfs-unlock` — Remove "locked" setting for a file on the Git LFS server

## Synopsis

```
git-lfs-unlock [OPTIONS] [PATHS]...
```

## Description

Remove "locked" setting for a file on the Git LFS server

Removes the given file path as "locked" on the Git LFS server. Files must exist and have a clean git status before they can be unlocked. The `--force` flag will skip these checks.

## Options

### Arguments

- `<PATHS>`
    Paths to unlock. Upstream's CLI accepts a single path; ours accepts multiple (additive extension). Mutually exclusive with `--id`

### Flags

- `-r`, `--remote` `<REMOTE>`
    Specify the Git LFS server to use. Ignored if the `lfs.url` config key is set

- `-f`, `--force`
    Tell the server to remove the lock, even if it's owned by another user

- `-i`, `--id` `<ID>`
    Specify a lock by its ID instead of path. Mutually exclusive with the positional paths

- `-j`, `--json`
    Write lock info as JSON to standard output if the command exits successfully.

    Intended for interoperation with external tools. If the command returns with a non-zero exit code, plain text messages are sent to standard error.

- `--ref` `<REFSPEC>`
    Refspec to send with the unlock request (extension over upstream).

    Defaults to the current branch's tracked upstream — same auto-resolution as `git lfs lock`.

## See also

[git-lfs-lock(1)](./git-lfs-lock.md), [git-lfs-locks(1)](./git-lfs-locks.md).

## Reporting bugs

This command is from the Rust implementation of git-lfs, not the original
Go implementation. Please report bugs to our [issue tracker](https://gitlab.com/rustutils/git-lfs/issues).
