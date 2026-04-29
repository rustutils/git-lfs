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

- `clean` — Run the clean filter: read content on stdin, write a pointer on stdout
- `smudge` — Run the smudge filter: read a pointer on stdin, write content on stdout
- `install` — Configure git to invoke git-lfs as the clean/smudge/process filter, and install the LFS git hooks
- `uninstall` — Reverse of `install`: clear the `filter.lfs.*` config and remove the LFS git hooks. Hooks that don't match what we'd write are left untouched
- `track` — Track a file pattern with git-lfs by adding it to .gitattributes. With no patterns, lists currently-tracked patterns
- `untrack` — Stop tracking a file pattern with git-lfs by removing it from .gitattributes. The matching pointer files in history (and the objects in the local store) are left in place
- `filter-process` — Run the long-running filter-process protocol with git over stdin/stdout. This is what git invokes via filter.lfs.process and is the batched alternative to per-invocation `clean`/`smudge`
- `fetch` — Download every LFS object reachable from the given refs (default: HEAD) that isn't already in the local store. Walks history, dedupes by OID
- `pull` — `fetch` then re-run the smudge filter so the working tree contains real LFS file contents instead of pointer text. Requires `git lfs install` to have wired up the smudge filter
- `push` — Upload every LFS object reachable from the given refs that the remote doesn't already have. The "doesn't have" set is approximated by `refs/remotes/<remote>/*`; the LFS server's batch API also dedupes server-side so missing exclusions don't waste bandwidth
- `clone` — Deprecated. Wraps `git clone` so the working tree is populated with pointer text first, then runs `git lfs pull` to download LFS content in batch. Modern `git clone` parallelizes the smudge filter and is no slower; prefer it
- `post-checkout` — Git post-checkout hook entry point. Receives `<prev-sha> <post-sha> <flag>` (flag is "1" if HEAD moved). Currently a no-op stub — exists so installed hook scripts don't fail. Real behavior arrives with `track --lockable`
- `post-commit` — Git post-commit hook entry point. No arguments. Currently a no-op stub
- `post-merge` — Git post-merge hook entry point. Receives `<squash-flag>`. Currently a no-op stub
- `pre-push` — Git pre-push hook entry point — not typically invoked by hand. Reads `<local-ref> <local-sha> <remote-ref> <remote-sha>` lines from stdin and uploads the LFS objects newly reachable from each `<local-sha>`
- `version` — Print the git-lfs version and exit
- `pointer` — Debug helper: build a pointer from a file, parse one from disk or stdin, or just check whether some bytes are a valid pointer
- `env` — Show the LFS environment: version, endpoints, on-disk paths, and the three `filter.lfs.*` config values
- `migrate` — Analyze or rewrite history for LFS conversion. Phase 1 ships `info` only; `import` and `export` will land in subsequent phases
- `checkout` — Replace pointer text in the working tree with actual LFS object content. With no args, materializes every LFS pointer in HEAD's tree. With paths (literal file names or trailing-slash directory prefixes), restricts to matching pointers
- `prune` — Delete local LFS objects that aren't reachable from HEAD or any unpushed commit. Reclaims disk for repos whose history has moved past their objects
- `fsck` — Check the integrity of LFS objects and pointers reachable from `<refspec>` (default: HEAD). Exit 1 if anything is corrupt
- `status` — Show staged + unstaged changes, classifying each blob as LFS, Git, or working-tree File
- `lock` — Acquire an exclusive server-side lock on one or more files. Other users will be unable to push changes to a locked file
- `locks` — List file locks held on the server
- `unlock` — Release a file lock previously acquired with `git lfs lock`. Either provide one or more paths, or `--id <id>` (mutually exclusive)
- `ls-files` — List LFS-tracked files visible at a ref (default: HEAD), or across all reachable history with `--all`

