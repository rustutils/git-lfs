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
- `filter-process` — Run the long-running filter-process protocol with git over stdin/stdout. This is what git invokes via filter.lfs.process and is the batched alternative to per-invocation `clean`/`smudge`
- `fetch` — Download all Git LFS files for a given ref
- `pull` — Download all Git LFS files for current ref and checkout
- `push` — Push queued large files to the Git LFS endpoint
- `clone` — Deprecated. Wraps `git clone` so the working tree is populated with pointer text first, then runs `git lfs pull` to download LFS content in batch. Modern `git clone` parallelizes the smudge filter and is no slower; prefer it
- `post-checkout` — Git post-checkout hook entry point. Receives `<prev-sha> <post-sha> <flag>` (flag is "1" if HEAD moved). Currently a no-op stub — exists so installed hook scripts don't fail. Real behavior arrives with `track --lockable`
- `post-commit` — Git post-commit hook entry point. No arguments. Currently a no-op stub
- `post-merge` — Git post-merge hook entry point. Receives `<squash-flag>`. Currently a no-op stub
- `pre-push` — Git pre-push hook entry point — not typically invoked by hand. Reads `<local-ref> <local-sha> <remote-ref> <remote-sha>` lines from stdin and uploads the LFS objects newly reachable from each `<local-sha>`
- `version` — Print the git-lfs version and exit
- `pointer` — Debug helper: build a pointer from a file, parse one from disk or stdin, or just check whether some bytes are a valid pointer
- `env` — Show the LFS environment: version, endpoints, on-disk paths, and the three `filter.lfs.*` config values
- `ext` — List the configured LFS pointer extensions (`lfs.extension.<name>.*`). Extensions chain external clean/smudge programs around each LFS object; this prints their resolved configuration in priority order
- `update` — (Re-)install the four LFS git hooks (`pre-push`, `post-checkout`, `post-commit`, `post-merge`) for the current repository
- `migrate` — Analyze or rewrite history for LFS conversion. Phase 1 ships `info` only; `import` and `export` will land in subsequent phases
- `checkout` — Populate working copy with real content from Git LFS files
- `prune` — Delete local LFS objects that aren't reachable from HEAD or any unpushed commit. Reclaims disk for repos whose history has moved past their objects
- `fsck` — Check the integrity of LFS objects and pointers reachable from `<refspec>` (default: HEAD). Exit 1 if anything is corrupt
- `status` — Show staged + unstaged changes, classifying each blob as LFS, Git, or working-tree File
- `lock` — Set a file as "locked" on the Git LFS server
- `locks` — Lists currently locked files from the Git LFS server
- `unlock` — Remove "locked" setting for a file on the Git LFS server
- `ls-files` — List LFS-tracked files visible at a ref (default: HEAD), or across all reachable history with `--all`

## Reporting bugs

This command is from the Rust implementation of git-lfs, not the original
Go implementation. Please report bugs to our [issue tracker](https://gitlab.com/rustutils/git-lfs/issues).
