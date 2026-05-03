# git-lfs-lock

## Name

`git-lfs-lock` — Set a file as "locked" on the Git LFS server

## Synopsis

```
git-lfs-lock [OPTIONS] [PATHS]...
```

## Description

Set a file as "locked" on the Git LFS server

Sets the given file path as "locked" against the Git LFS server, with the intention of blocking attempts by other users to update the given path. Locking a file requires the file to exist in the working copy.

Once locked, LFS will verify that Git pushes do not modify files locked by other users. See the description of the `lfs.<url>.locksverify` config key in [git-lfs-config(5)](./git-lfs-config.md) for details.

## Options

### Arguments

- `<PATHS>`
    Paths to lock. Repo-relative or absolute; must resolve inside the working tree. Upstream's CLI accepts a single path; ours accepts multiple (additive extension)

### Flags

- `-r`, `--remote` `<REMOTE>`
    Specify the Git LFS server to use. Ignored if the `lfs.url` config key is set

- `-j`, `--json`
    Write lock info as JSON to standard output if the command exits successfully.

    Intended for interoperation with external tools. If the command returns with a non-zero exit code, plain text messages are sent to standard error.

- `--ref` `<REFSPEC>`
    Refspec to associate the lock with (extension over upstream).

    Defaults to the current branch's tracked upstream (`branch.<current>.merge`) or the current branch's full ref (`refs/heads/<branch>`).

## See also

[git-lfs-unlock(1)](./git-lfs-unlock.md), [git-lfs-locks(1)](./git-lfs-locks.md).

## Reporting bugs

This command is from the Rust implementation of git-lfs, not the original
Go implementation. Please report bugs to our [issue tracker](https://gitlab.com/rustutils/git-lfs/issues).
