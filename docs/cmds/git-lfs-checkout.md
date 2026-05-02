# git-lfs-checkout

## Name

`git-lfs-checkout` — Populate working copy with real content from Git LFS files

## Synopsis

```
git-lfs-checkout [OPTIONS] [PATHS]...
```

## Description

Populate working copy with real content from Git LFS files.

Replace pointer text in the working tree with actual LFS object content. With no args, materializes every LFS pointer in HEAD's tree. With paths (literal file names or trailing-slash directory prefixes), restricts to matching pointers.

During a merge conflict, `--to <path> --ours/--theirs/--base <file>` writes the LFS content from one of the conflicted stages to `<path>` (creating intermediate directories) so the user can compare or salvage versions.

## Options

### Arguments

- `<PATHS>`
    Paths to check out. Empty = everything in HEAD's tree. In conflict mode (`--to`), exactly one path is required

### Flags

- `--to` `<PATH>`
    Conflict-mode: write the chosen stage's content to this path instead of into the working tree. Resolves relative to the current directory

- `--ours`
    Conflict-mode: pull from stage 2 (HEAD's version). Mutually exclusive with `--theirs` and `--base`

- `--theirs`
    Conflict-mode: pull from stage 3 (the merging-in version)

- `--base`
    Conflict-mode: pull from stage 1 (the common ancestor)

