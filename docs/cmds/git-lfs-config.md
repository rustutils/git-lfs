# git-lfs-config

## Name

`git-lfs-config` — Configuration options for git-lfs

## Synopsis

```
git-lfs-config
```

## Description

Configuration options for git-lfs

## Configuration files

git-lfs reads its configuration from any file `git config -l` returns — that is, the system, global, and per-repository Git config files in their usual precedence order.

A small subset of keys may also be set in a `.lfsconfig` file at the repository root; see LFSCONFIG for the format and the list of keys allowed there. This is useful for settings every clone of the repository should share — most commonly `lfs.url` or an access mode — without forcing each user to configure them manually.

If `.lfsconfig` is missing on disk, the index is checked for it. If that's also missing, `HEAD` is checked. In a bare repository, only `HEAD` is checked.

Settings from Git config files override `.lfsconfig`. This lets you change an LFS-related setting locally (e.g. point `lfs.url` at a staging server) without modifying the repository's tracked configuration.

Most LFS settings live in the `[lfs]` section — keys of the form `lfs.<foo>`. A handful are scoped inside a particular remote's config (`remote.<name>.lfsurl` and similar) and override the global `lfs.*` equivalents for that remote.

URL-specific overrides are written as `lfs.<url>.<key>`, where `<url>` is the LFS endpoint the setting should apply to. Longest-prefix match wins, so `lfs.https://lfs.example.com/.locksverify` overrides `lfs.locksverify` only for that endpoint.

## Lfsconfig

`.lfsconfig` at the repository root uses the same format as `.git/config`. Only a restricted set of keys is honored here (the others are silently ignored), for security: a `.lfsconfig` from an untrusted clone shouldn't be able to override credential helpers or arbitrary Git config.

Allowed keys:

- `lfs.allowincompletepush`
- `lfs.fetchexclude`
- `lfs.fetchinclude`
- `lfs.gitprotocol`
- `lfs.locksverify`
- `lfs.pushurl`
- `lfs.skipdownloaderrors`
- `lfs.url`
- `lfs.<url>.access`
- `remote.<name>.lfsurl`

## Examples

Configure a custom LFS endpoint for everyone who clones the repository:

    git config -f .lfsconfig lfs.url https://lfs.example.com/foo/bar/info/lfs

Set the endpoint locally for the current user without touching `.lfsconfig`:

    git config --global lfs.url https://lfs.example.com/foo/bar/info/lfs

Disable lock verification at the pre-push hook for a specific endpoint:

    git config --global lfs.https://lfs.example.com/.locksverify false

Raise the concurrent-transfer ceiling on a fast link:

    git config --global lfs.concurrenttransfers 16

Exclude a large media subtree from fetch/checkout:

    git config lfs.fetchexclude "media/raw/**,**/*.psd"

## See also

git-lfs(1), git-config(1), [gitattributes(5)](https://git-scm.com/docs/gitattributes), [gitignore(5)](https://git-scm.com/docs/gitignore).

## Reporting bugs

This command is from the Rust implementation of git-lfs, not the original
Go implementation. Please report bugs to our [issue tracker](https://gitlab.com/rustutils/git-lfs/issues).
