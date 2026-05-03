# git-lfs-clone

## Name

`git-lfs-clone` — Efficiently clone a LFS-enabled repository

## Synopsis

```
git-lfs-clone [ARGS]...
```

## Description

Efficiently clone a LFS-enabled repository

Clone an LFS-enabled Git repository by disabling LFS during the `git clone`, then running `git lfs pull` directly afterwards. Also installs the repo-level hooks (`.git/hooks`) that LFS requires to operate; if `--separate-git-dir` is given to `git clone`, the hooks are installed there.

Historically faster than a regular `git clone` because that would download LFS content via the smudge filter one file at a time. Modern `git clone` parallelizes the smudge filter, so this command no longer offers a meaningful speedup over plain `git clone`. You should prefer plain `git clone`.

In addition to the options accepted by `git clone`, the LFS-only flags `--include` / `-I <paths>`, `--exclude` / `-X <paths>`, and `--skip-repo` (skip installing the repo-level hooks) are accepted — see [git-lfs-fetch(1)](./git-lfs-fetch.md) for the include/exclude semantics. They're parsed from the trailing argument list rather than declared as clap flags, so they don't appear in this command's `--help`.

## Options

### Arguments

- `<ARGS>`
    `git clone` arguments plus the LFS pass-through flags (`-I`/`--include`, `-X`/`--exclude`, `--skip-repo`). The repository URL is required; an optional target directory follows

## See also

[git-clone(1)](https://git-scm.com/docs/git-clone), [git-lfs-pull(1)](./git-lfs-pull.md), [gitignore(5)](https://git-scm.com/docs/gitignore).

## Reporting bugs

This command is from the Rust implementation of git-lfs, not the original
Go implementation. Please report bugs to our [issue tracker](https://gitlab.com/rustutils/git-lfs/issues).
