# git-lfs

## Name

`git-lfs` — Git LFS — large file storage for git

## Synopsis

```
git-lfs [COMMAND]
```

## Description

Git LFS — large file storage for git

## Options

### Flags

- `-V`, `--version`
    Print the version banner and exit

### Subcommands

- `clean` — Git clean filter that converts large files to pointers
- `smudge` — Git smudge filter that converts pointer in blobs to the actual content
- `install` — Install Git LFS configuration
- `uninstall` — Remove Git LFS configuration
- `track` — View or add Git LFS paths to Git attributes
- `untrack` — Remove Git LFS paths from Git attributes
- `filter-process` — Git filter process that converts between pointer and actual content
- `fetch` — Download all Git LFS files for a given ref
- `pull` — Download all Git LFS files for current ref and checkout
- `push` — Push queued large files to the Git LFS endpoint
- `clone` — Efficiently clone a LFS-enabled repository
- `post-checkout` — Git post-checkout hook implementation
- `post-commit` — Git post-commit hook implementation
- `post-merge` — Git post-merge hook implementation
- `pre-push` — Git pre-push hook implementation
- `version` — Print the git-lfs version banner and exit
- `pointer` — Build, compare, and check pointers
- `env` — Display the Git LFS environment
- `ext` — List the configured LFS pointer extensions
- `update` — Update Git hooks
- `migrate` — Migrate history to or from Git LFS
- `checkout` — Populate working copy with real content from Git LFS files
- `prune` — Delete old LFS files from local storage
- `fsck` — Check Git LFS files for consistency
- `status` — Show the status of Git LFS files in the working tree
- `lock` — Set a file as "locked" on the Git LFS server
- `locks` — Lists currently locked files from the Git LFS server
- `unlock` — Remove "locked" setting for a file on the Git LFS server
- `ls-files` — Show information about Git LFS files in the index and working tree

## Reporting bugs

This command is from the Rust implementation of git-lfs, not the original
Go implementation. Please report bugs to our [issue tracker](https://gitlab.com/rustutils/git-lfs/issues).
