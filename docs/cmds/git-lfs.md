# git-lfs

## Name

`git-lfs` — Git LFS — large file storage for git

## Synopsis

```
git-lfs [COMMAND]
```

## Description

Git LFS is a system for managing and versioning large files in association with a Git repository. Instead of storing the large files within the Git repository as blobs, Git LFS stores special "pointer files" in the repository, while storing the actual file contents on a Git LFS server. The contents of the large file are downloaded automatically when needed, for example when a Git branch containing the large file is checked out.

Git LFS works by using a "smudge" filter to look up the large file contents based on the pointer file, and a "clean" filter to create a new version of the pointer file when the large file's contents change. It also uses a pre-push hook to upload the large file contents to the Git LFS server whenever a commit containing a new large file version is about to be pushed to the corresponding Git server.

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
- `logs` — Show errors logged by Git LFS

## Examples

To get started with Git LFS, the following commands can be used.

1. Setup Git LFS on your system. You only have to do this once per user account:

   ```
   git lfs install
   ```

2. Choose the type of files you want to track, for examples all ISO images, with [git-lfs-track(1)](./git-lfs-track.md):

   ```
   git lfs track "*.iso"
   ```

3. The above stores this information in [gitattributes(5)](https://git-scm.com/docs/gitattributes) files, so that file needs to be added to the repository:

   ```
   git add .gitattributes
   ```

4. Commit, push and work with the files normally:

   ```
   git add file.iso
   git commit -m "Add disk image"
   git push
   ```

## Reporting bugs

This command is from the Rust implementation of git-lfs, not the original
Go implementation. Please report bugs to our [issue tracker](https://gitlab.com/rustutils/git-lfs/issues).
