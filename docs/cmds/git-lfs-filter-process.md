# git-lfs-filter-process

## Name

`git-lfs-filter-process` — Git filter process that converts between pointer and actual content

## Synopsis

```
git-lfs-filter-process [OPTIONS]
```

## Description

Git filter process that converts between pointer and actual content

Implement the Git process filter API, exchanging handshake messages and then accepting and responding to requests to either clean or smudge a file.

`filter-process` is always run by Git's filter process, and is configured by the repository's Git attributes.

In your Git configuration or in a `.lfsconfig` file, you may set either or both of `lfs.fetchinclude` and `lfs.fetchexclude` to comma-separated lists of paths. If `lfs.fetchinclude` is defined, Git LFS pointer files will only be replaced with the contents of the corresponding object file if their path matches one in that list, and if `lfs.fetchexclude` is defined, pointer files will only be replaced if their path does not match one in that list. Paths are matched using wildcard matching as per [gitignore(5)](https://git-scm.com/docs/gitignore). Pointer files that are not replaced are simply copied to standard output without change.

The filter process uses Git's pkt-line protocol to communicate, and is documented in detail in [gitattributes(5)](https://git-scm.com/docs/gitattributes).

## Options

### Flags

- `-s`, `--skip`
    Skip automatic downloading of objects on clone or pull.

    Equivalent to `GIT_LFS_SKIP_SMUDGE=1`. Wired up by `git lfs install --skip-smudge`.

## See also

[git-lfs-clean(1)](./git-lfs-clean.md), [git-lfs-install(1)](./git-lfs-install.md), [git-lfs-smudge(1)](./git-lfs-smudge.md), [gitattributes(5)](https://git-scm.com/docs/gitattributes), [gitignore(5)](https://git-scm.com/docs/gitignore).

## Reporting bugs

This command is from the Rust implementation of git-lfs, not the original
Go implementation. Please report bugs to our [issue tracker](https://gitlab.com/rustutils/git-lfs/issues).
