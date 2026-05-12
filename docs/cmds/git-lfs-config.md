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

## Push settings

- `lfs.allowincompletepush`

  When `true`, allow a push to complete even if some LFS objects are missing from the local cache. By default (false), pre-push aborts and the user has to resolve the gap before pushing.

- `lfs.<url>.locksverify` (or unscoped `lfs.locksverify`)

  Controls whether the pre-push hook calls the lock API on the LFS endpoint to refuse pushes over files locked by someone else.

  - `true`: verify locks; halt the push if any are violated or the server is unreachable.
  - `false`: skip the lock check entirely. Set this if you don't use file locking, or your server enforces it server-side.
  - Unset: attempt the call; if it succeeds, persist `true` for next time. If the server returns 501 Not Implemented, persist `false`. If it fails for another reason, warn and continue. (Matches upstream's first-call probe.)

## Fetch settings

- `lfs.fetchinclude`

  Comma-separated list of `gitignore(5)`-style patterns. When set, fetch only downloads objects whose path matches one of them. Empty string disables the filter.

- `lfs.fetchexclude`

  Inverse of `fetchinclude` — fetch skips objects whose path matches.

- `lfs.fetchrecentrefsdays`

  Branches whose tip commit lies within this many days of now are included by `fetch --recent`. Only local refs are scanned unless `lfs.fetchrecentremoterefs` is also set. Default 7. A value of 0 disables ref-window retention entirely.

- `lfs.fetchrecentremoterefs`

  When `true`, `fetch --recent` also scans the remote-tracking refs of the remote being fetched (useful for picking up branches you might check out later without first creating a tracking local ref). Default `true`.

- `lfs.fetchrecentcommitsdays`

  In addition to fetching the tip state of each recent ref, also fetch LFS objects referenced by commits within this many days of that ref's tip. Default 0 (tip only).

- `lfs.fetchrecentalways`

  When `true`, always behave as if `--recent` was passed. Default `false`.

## Prune settings

- `lfs.pruneoffsetdays`

  Extra days added to the `lfs.fetchrecent*days` windows when deciding what prune can delete. A ref or commit has to be at least this many days older than the oldest one `fetch --recent` would download for prune to treat it as old enough to delete. Default 3. Only takes effect when the underlying fetch-recent setting is non-zero.

- `lfs.pruneremotetocheck`

  Remote to consult for UNPUSHED LFS FILES detection and `--verify-remote`. Default `origin`. See [git-lfs-prune(1)](./git-lfs-prune.md) for the full retention rules.

- `lfs.pruneverifyremotealways`

  When `true`, always run prune as if `--verify-remote` was passed. The pre-delete remote-presence check applies on every invocation. Use `--no-verify-remote` to opt out for a single run.

- `lfs.pruneverifyunreachablealways`

  When `true`, always run prune as if `--verify-unreachable` was passed — also verify objects not reachable from any commit. Only meaningful when remote verification is on. Use `--no-verify-unreachable` to opt out for a single run.

## Extensions

Git LFS extensions wrap each object's bytes through an external program on the clean (commit) and smudge (checkout) paths — useful for repository-wide transforms like compression or encryption that should happen alongside pointerization.

- `lfs.extension.<name>.clean`

  Command run when files are added to the index. Receives the raw bytes on stdin and is expected to emit transformed bytes on stdout.

- `lfs.extension.<name>.smudge`

  Command run when files are written into the working copy. Reverses what `clean` produced.

- `lfs.extension.<name>.priority`

  Sort order across extensions. Lower priorities run first on the clean side, last on the smudge side (so a chain `compress -> encrypt` reverses to `decrypt -> decompress`). Required when more than one extension is configured.

See `git-lfs-ext(1)` for inspecting the resolved chain.

## Other settings

- `lfs.setlockablereadonly` / `GIT_LFS_SET_LOCKABLE_READONLY`

  Whether files tracked as `lockable` in `.gitattributes` are made read-only in the working copy unless the current user holds the lock. Default `true`. Set either to `0` / `false` / `no` to keep them writeable.

- `lfs.skipdownloaderrors` / `GIT_LFS_SKIP_DOWNLOAD_ERRORS`

  Don't abort the smudge filter when an LFS download fails. The pointer is left in the working tree as-is, and the surrounding `git checkout` (or whatever invoked smudge) reports success. Useful when you need to operate on a repository whose remote is temporarily unavailable, but be aware that scripts checking smudge exit status won't see the failure.

- `GIT_LFS_SKIP_SMUDGE`

  Skip pointer-to-content conversion in `git lfs smudge` and `git lfs filter-process`. Equivalent to running `git lfs install --skip-smudge` (which sets it via `filter.lfs.process`). Any value other than empty / `0` / `false` enables it.

- `GIT_LFS_SKIP_PUSH`

  Make the pre-push hook a no-op. New LFS objects are not uploaded for the duration of the command. Same value semantics as `GIT_LFS_SKIP_SMUDGE`.

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
