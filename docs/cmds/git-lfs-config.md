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

## General settings

- `lfs.url` / `remote.<remote>.lfsurl`

  The URL of the Git LFS API endpoint. Defaults to deriving the endpoint from the clone URL (`<clone-url>/info/lfs`). The remote-scoped form overrides the general one for a particular remote.

- `lfs.pushurl` / `remote.<remote>.lfspushurl`

  Same idea but consulted only when pushing. Defaults to `lfs.url` or the derived endpoint.

- `lfs.<url>.access`

  Authentication mode for the LFS endpoint at `<url>`. Either `basic` (HTTP basic auth via the credential helper, the default after a successful round-trip) or `none` (no authentication). Set via `git config --add` when the access mode for an endpoint should be persisted; the auth-retry loop also writes this on a successful 401-fill cycle.

- `core.askpass` / `GIT_ASKPASS`

  Program invoked when interactive credentials are needed against the LFS API. Stdout is read as the credential value. Same selection priority as Git uses: `GIT_ASKPASS` env beats `core.askpass`, which beats `SSH_ASKPASS`.

- `credential.helper`, `credential.useHttpPath`, `credential.protectProtocol`

  Standard Git credential-helper plumbing. `useHttpPath=true` distinguishes credentials per path within a host (so two paths on the same domain can have different passwords). `protectProtocol=false` lets credentials with carriage returns through (default `true`).

## Upload and download transfer settings

- `lfs.concurrenttransfers`

  Number of object transfers running in parallel within a single LFS command. Default 8.

- `lfs.basictransfersonly`

  When `true`, restrict the client to the basic HTTP upload/download adapter, ignoring more advanced transfers the server may advertise. Useful for working around broken intermediaries. Default `false`.

- `lfs.transfer.batchSize`

  Max objects per `POST /objects/batch` request. The transfer queue chunks the input list into runs of this size and issues one batch call per chunk. Default 100. Values < 1 are clamped to 1. Servers may refuse oversize batches with 413; lower this if you see those.

- `lfs.transfer.enablehrefrewrite`

  When `true`, applies `url.<base>.insteadOf` / `url.<base>.pushInsteadOf` rewrites to the action URLs the batch endpoint hands back. `pushInsteadOf` is used for upload actions; `insteadOf` is used for downloads and for uploads when `pushInsteadOf` isn't set. Default `false`.

- `lfs.<url>.contenttype`

  When `true` (the default), the basic upload adapter sniffs the first 512 bytes of each object and sets the `Content-Type` header on the PUT to the detected MIME type. Set to `false` to send `application/octet-stream` unconditionally — useful when a CDN rejects uploads based on content sniffing. The batch response's `action.header` always wins if it pins a Content-Type itself.

- `lfs.<url>.sshtransfer`

  Whether to use SSH (`git-lfs-authenticate`) for the LFS endpoint at `<url>`. Values: `negotiate` (try SSH first, fall back to HTTPS — the default for `ssh://` and `git@` remotes), `always`, or `never`.

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
