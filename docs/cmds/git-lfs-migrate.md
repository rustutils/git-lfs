# git-lfs-migrate

## Name

`git-lfs-migrate` — Analyze or rewrite history for LFS conversion. Phase 1 ships `info` only; `import` and `export` will land in subsequent phases

## Synopsis

```
git-lfs-migrate <COMMAND>
```

## Description

Analyze or rewrite history for LFS conversion. Phase 1 ships `info` only; `import` and `export` will land in subsequent phases

## Options

### Subcommands

- `import` — Rewrite history so files matching the include filter become LFS pointers. With `--no-rewrite`, history is preserved and one new commit is appended on top of HEAD with the named paths converted in place
- `export` — Inverse of import: rewrite history so LFS pointers become the raw bytes they reference. Requires the LFS objects to already be in the local store — `git lfs fetch` first if not. Pointers whose objects are missing are left as-is
- `info` — Walk history and report file extensions by total size. Read-only — no objects or history change

