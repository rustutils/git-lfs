# git-lfs-track

## Name

`git-lfs-track` — Track a file pattern with git-lfs by adding it to .gitattributes. With no patterns, lists currently-tracked patterns

## Synopsis

```
git-lfs-track [OPTIONS] [PATTERNS]...
```

## Description

Track a file pattern with git-lfs by adding it to .gitattributes. With no patterns, lists currently-tracked patterns

## Options

### Arguments

- `<PATTERNS>`
    File patterns to track (e.g. "*.jpg", "data/*.bin")

### Flags

- `-l`, `--lockable`
    Mark the tracked pattern as `lockable` (`*.psd lockable`)

- `--not-lockable`
    Re-track an existing pattern, removing its `lockable` flag

- `--dry-run`
    Print what would happen without modifying `.gitattributes` or re-staging files

- `-v`, `--verbose`
    Extra logging: print "Found N files previously added to Git matching pattern" lines

- `--json`
    Listing mode only: emit JSON instead of the human-readable listing

- `--no-excluded`
    Listing mode only: suppress the "Listing excluded patterns" section

