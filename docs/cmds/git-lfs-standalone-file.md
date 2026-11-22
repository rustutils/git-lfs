# git-lfs-standalone-file

## Name

`git-lfs-standalone-file` — Standalone transfer adapter for file URLs

## Synopsis

```
git-lfs-standalone-file
```

## Description

Standalone transfer adapter for file URLs

Copy LFS objects to and from another repository on this machine, for a remote with a `file://` URL or a local path. Git LFS runs this itself, as the `lfs-standalone-file` transfer adapter, so no configuration is needed.

It speaks the custom transfer protocol as JSON on standard input and output, and isn't meant to be run by hand.

## Reporting bugs

This command is from the Rust implementation of git-lfs, not the original
Go implementation. Please report bugs to our [issue tracker](https://gitlab.com/rustutils/git-lfs/issues).
