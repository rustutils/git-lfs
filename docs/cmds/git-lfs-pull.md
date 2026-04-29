# git-lfs-pull

## Name

`git-lfs-pull` — `fetch` then re-run the smudge filter so the working tree contains real LFS file contents instead of pointer text. Requires `git lfs install` to have wired up the smudge filter

## Synopsis

```
git-lfs-pull [OPTIONS] [REFS]...
```

## Description

`fetch` then re-run the smudge filter so the working tree contains real LFS file contents instead of pointer text. Requires `git lfs install` to have wired up the smudge filter

## Options

### Arguments

- `<REFS>`
    Refs to scan for LFS pointers. Defaults to `HEAD`

### Flags

- `-I`, `--include` `<INCLUDE>`
    Comma-separated globs; only matching paths are pulled. Falls back to `lfs.fetchinclude` when omitted

- `-X`, `--exclude` `<EXCLUDE>`
    Comma-separated globs; matching paths are skipped. Falls back to `lfs.fetchexclude` when omitted

