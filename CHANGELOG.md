# Changelog

All notable changes to this project will be documented in this file.

The format is based on [Keep a
Changelog](https://keepachangelog.com/en/1.1.0/), and this project adheres to
[Semantic Versioning](https://semver.org/spec/v2.0.0.html).

## [Unreleased]

### Security

- `.lfsconfig` values containing a newline can no longer add other settings,
  such as `lfs.url`. A cloned repository could use this to change where LFS
  objects are sent.
- Batch, lock, download, and verify requests follow redirects with
  upstream's rules: credentials aren't sent on after a redirect to another
  origin, including from HTTP to HTTPS on the same host and port, and
  redirects from HTTPS to HTTP are refused. Uploads already worked this way.
- An SSH host starting with `-`, such as `-oProxyCommand=...` in an `ssh://`
  URL, is no longer read by ssh as an option when running
  `git-lfs-authenticate`.
- `.lfsconfig` can only set `access` keys under `lfs.`, such as
  `lfs.<url>.access`, as in upstream. Other keys ending in `.access` are
  ignored with a warning.
- Objects in a batch response that weren't requested are skipped with
  "The server returned an unknown OID.", like upstream. A server could
  otherwise get any local object whose OID it knew uploaded to it.

### Added

- `lfs.tusTransfers` offers the server resumable uploads with the tus.io
  protocol. An interrupted upload resumes where the server stopped.
- `GIT_LFS_PROGRESS` names a file that uploads, downloads, checkouts, and
  the clean and smudge filters append progress lines to, in upstream's
  format.
- Downloads ask for gzip-compressed responses, as upstream does. Set
  `lfs.transfer.httpDownloadEncoding`, or
  `lfs.transfer.<url>.httpDownloadEncoding`, to `zstd` to ask for zstd
  instead. Batch API requests also accept gzip.
- `lfs.remote.autodetect` downloads objects for a checkout from the remote
  whose branch contains the commit being checked out, and
  `lfs.remote.searchall` tries every remote when a download fails. They
  help when commits come from more than one remote, such as a fork.
- Custom transfer adapters, set up with `lfs.customtransfer.<name>.path`,
  `args`, `concurrent`, and `direction`. They're offered to the server in
  each batch request, and used when the server picks one.
  `lfs.basictransfersonly` offers only `basic`.
- Remotes with a `file://` URL or a local path work for fetching and
  pushing LFS objects, through the built-in `lfs-standalone-file` adapter
  and the new `git lfs standalone-file` command.
- `lfs.standaloneTransferAgent` and `lfs.<url>.standaloneTransferAgent`
  name a custom adapter that moves objects without an LFS server. No batch
  request or lock check is sent.
- `git lfs ls-files` takes `--include` and `--exclude` patterns, and a
  second ref: `git lfs ls-files <a> <b>` lists the LFS files changed in the
  commits after `<a>` up to `<b>`. Its usage errors now go to standard
  error and exit 2, as in upstream.
- `git lfs dedup` replaces LFS files in the working tree with
  copy-on-write clones of their stored objects, on file systems that
  support it, such as Btrfs, XFS, APFS, and ReFS. `--test` only checks for
  support.
- `git lfs completion bash|fish|zsh` prints a shell completion script for
  `git lfs` and `git-lfs`. It completes commands, options, and the values
  some options take. Scripts installed from upstream Git LFS work too.
- Pushes use `lfs.pushurl`, `remote.<name>.lfspushurl`, and
  `remote.<name>.pushurl` when set, and `url.<base>.pushInsteadOf` aliases.
- The lock commands, and lock verification on push, use the push remote
  (`branch.<name>.pushRemote`, `remote.lfspushdefault`, or
  `remote.pushDefault`) and its push URL, like upstream.
- `core.sshCommand` is read when neither `GIT_SSH_COMMAND` nor `GIT_SSH` is
  set.
- `lfs.transfer.maxRetryTime` limits how long a `Retry-After` delay is
  waited for, 300 seconds by default. A server asking for longer fails the
  transfer instead.
- When an SSH server has no `git-lfs-authenticate`, requests go to the
  HTTPS endpoint guessed from the SSH URL, as the server discovery spec
  says, instead of failing.
- `lfs.transfer.maxRetries` and `lfs.transfer.maxRetryDelay` set the number
  of retries and the longest delay between them. The defaults are
  upstream's: 8 retries, with a delay starting at 250 milliseconds and
  doubling up to 10 seconds. It used to start at 100 milliseconds and go up
  to 30 seconds.

### Changed

- `git lfs migrate import` and `export` ask before overwriting
  uncommitted changes, like upstream, instead of refusing. `--yes` (now
  also `-y`) skips the question, and `export` now honors it too.
- A failed object is reported as `[<oid>] <error>`, like upstream.
- `GIT_TRACE` output has upstream's `HTTP: <method> <url>` line for each
  request and `HTTP: <status>` line for each response.
- `exec:` lines in `GIT_TRACE` output quote each argument, like upstream, so
  they show exactly what was run.
- `GIT_SSH_COMMAND` and `core.sshCommand` run with `sh -c`, like Git and
  upstream, so they can hold quoted arguments. `GIT_SSH` is run as one
  path, so it can contain spaces. Both were split on whitespace before.
- The path of an `ssh://` URL is percent-decoded, like upstream, so
  `ssh://host/my%20repo` refers to `/my repo`.
- Error messages match upstream's wording: a corrupt download says how
  many bytes were written, a batch response that isn't valid JSON says
  "Unable to parse HTTP response for POST <url>", and failed `fetch` and
  `pull` runs name the endpoint, as in "Failed to fetch some objects from
  '<url>'".
- A failed smudge download prints upstream's "Error downloading object"
  line with the cause. The `smudge` command then writes the pointer and
  exits 2, like upstream, and with `lfs.skipDownloadErrors` it exits 0.
  Under `git checkout` and `git pull`, Git only reported "smudge filter lfs
  failed" before.
- `GIT_TRACE` output has upstream's `tq: starting transfer adapter` line.
- A batch request that fails to connect is sent again at once, up to
  `lfs.transfer.maxRetries` times, like upstream, instead of being retried
  with backoff. Over SSH this no longer runs `git-lfs-authenticate` again
  for each attempt.

### Fixed

- Uploads, downloads, and verify requests that get a 401 are sent again
  with credentials, as upstream does. They come from the LFS or remote URL
  when it's on the same host, or else from the credential helpers.
  `lfs.<url>.access` is then set to `basic` for the storage URL, so later
  runs send credentials up front.
- With `GIT_LFS_FORCE_PROGRESS`, transfers print their meter's starting
  line too, and `git lfs fetch` prints its line when the batch request
  fails, as upstream does.
- `git lfs fsck` checks the objects in the ref's tree, not in its whole
  history, as in upstream. `git lfs fsck a..b` still checks the commits in
  the range.
- Pushing to a local path fails when an object is missing locally, and
  respects `--dry-run`, as for other remotes.
- `git lfs ls-files` no longer lists empty files, as in upstream.
- `git lfs track` prints `Touching` lines only with `--verbose` or
  `--dry-run`, as upstream does.
- `git lfs track` and `untrack` pick the line ending for `.gitattributes`
  like upstream: the file's own, if it already has `filter` or `lockable`
  lines, then CRLF if `core.autocrlf` is true, then the platform's. On
  Windows, that's now CRLF by default.
- `git lfs migrate import` and `export` handle the same file content at two
  paths when only one of them matches `--include` and `--exclude`. Before,
  the first path decided for both. With `--above`, each path gets its own
  `.gitattributes` line.
- `git lfs env` warns when an `lfs.*` setting is set more than once with
  different values, as upstream does.
- With `http.cookieFile` set to an empty file, cookies that the server sets
  are kept and sent back, as upstream does. Before, they were dropped.
- An `href` from `git-lfs-authenticate` containing a control character,
  such as a newline, is rejected like upstream. It used to be silently
  stripped.
- `push` prints its progress line even when the whole upload fails, and the
  percentage in progress lines is padded to three characters, like
  upstream.
- `GIT_CURL_VERBOSE` output no longer shows a `Content-Length` for uploads
  the server asked to be chunked. None was sent.
- A push over pure SSH uses one SSH session for lock verification and
  uploads, like upstream, instead of opening a second one.

## [0.8.0] - 2026-11-21

### Security

- With `http.sslCAInfo` set, the server must prove it holds the private key
  of the certificate in that file. Before, any server presenting a copy of
  the certificate was accepted.
- SSH remotes quote the repository path for the server's shell, as upstream
  does. Before, a path containing shell syntax, which a cloned repository's
  `.lfsconfig` can set through `lfs.url`, could run commands on the server.
- When an upload is redirected to another host, the upload's credentials are
  no longer sent there, and redirects from HTTPS to HTTP are refused, as in
  upstream.
- `http.<url>.extraHeader` values are only sent to URLs they match. Before,
  headers scoped to the LFS server were also sent to the storage hosts that
  uploads and downloads go to.
- An `http.sslVerify` value that isn't a boolean, such as a typo, no longer
  turns off certificate verification. A warning names the value. Upstream
  turns verification off in this case.
- Passwords in remote URLs no longer appear in error messages or in
  `git lfs env`.
- `GIT_CURL_VERBOSE` output masks Basic credentials in all headers, including
  those from `http.extraHeader` and transfer actions, as upstream does. Set
  `LFS_DEBUG_HTTP` to show them.
- Pure-SSH downloads are written to a temporary file instead of memory. A
  server announcing a huge object size could crash the client.

### Added

- Pure-SSH transfers. For SSH remotes whose server provides `git-lfs-transfer`,
  uploads, downloads, and locking all run over SSH, with no HTTPS endpoint
  needed. Configure with `lfs.<url>.sshtransfer` (see Changed below).
- Credential helpers can return credentials with an auth type, such as Bearer
  tokens, and can run multistage authentication handshakes.
- The server's WWW-Authenticate headers are passed on to credential helpers.
  Set `credential.skipwwwauth` to turn this off.
- When the server rejects credentials, git-lfs asks for new ones, so the next
  helper gets a turn. After three attempts it fails with *too many
  authentication attempts*.
- `lfs.concurrentTransfers` now limits the number of parallel transfers.
- `lfs.transfer.maxverifies` sets how often a failed verify request is retried
  (default and minimum 3).
- Uploads follow HTTP redirects.
- `git lfs track` sets `lfs.repositoryformatversion` to 0 if it's unset, and
  refuses to run if it's set to anything else.
- `GIT_CURL_VERBOSE` output includes the Authorization header (masked), so you
  can confirm credentials were sent.

### Changed

- Building from source needs Rust 1.95 or newer.
- SSH remotes now try pure-SSH transfers first, and fall back to
  `git-lfs-authenticate` and HTTPS if the server doesn't support them. This
  matches upstream. Set `lfs.<url>.sshtransfer` to `never` for the old
  behavior, or to `always` to disable the fallback.
- Approved and rejected credentials now go only to the first credential helper
  that handles them, not to every helper, matching upstream.

### Fixed

- `git lfs migrate import` no longer converts `.gitattributes` files in
  subdirectories into LFS pointers.
- `git lfs migrate --everything` no longer rewrites the stash or notes,
  bisect, and replace refs, matching upstream.
- `git lfs migrate import` keeps non-ASCII file names. Before, a file like
  `café.bin` was renamed to `caf\303\251.bin` in the rewritten history.
- `git lfs migrate import --fixup` and `--no-rewrite` refuse to run with
  uncommitted changes unless `--yes` is given, like the other modes and
  upstream. `--fixup` used to discard them.
- Hooks go where Git runs them when `core.hooksPath` is set in the global or
  system config. Before, they went to `.git/hooks`, so the pre-push hook
  never ran and pushes left LFS objects behind. A relative `core.hooksPath`
  in a bare repository is resolved inside the repository, as Git does.
- `git lfs prune` refuses to run when a worktree is on a branch with no
  commits. Before, `--force` deleted the objects of staged files.
- `git lfs prune` no longer applies `lfs.fetchinclude`, which made it delete
  objects that the current checkout needs. As in upstream, only
  `lfs.fetchexclude` affects what it keeps.
- `git lfs prune --verify-remote` checks the remote set by
  `lfs.pruneremotetocheck`, as its documentation says, instead of the
  default remote.
- `git lfs prune` no longer tries to delete the null device when the store
  holds a file named after the empty object.
- `git lfs fetch` with refs lists objects in the same order on every run.

## [0.7.0] - 2026-05-13

### Added

- `git lfs merge-driver`, a Git merge driver for LFS files. It merges the file
  contents and stores the result as a new pointer.
- `git lfs logs`, for viewing and clearing crash logs.
- `git lfs clone --recursive` pulls LFS content in submodules too.
- Objects in the LFS store respect `core.sharedRepository`.
- `http.cookieFile` is honored, for servers and proxies that require a session
  cookie.
- Expired upload and download links are reported as expired, instead of being
  sent to the server.
- `url.<base>.pushInsteadOf` applies to upload URLs when
  `lfs.transfer.enablehrefrewrite` is set.
- `git lfs ls-files --deleted` lists files deleted in earlier commits.

### Fixed

- `git lfs ls-files` lists staged files that aren't committed yet, and works
  from subdirectories.
- `git lfs ls-files --all` with a ref argument fails with an error instead of
  ignoring the ref.
- `git lfs ls-files --json` output matches upstream's formatting.
- `git lfs smudge` honors `lfs.fetchinclude` and `lfs.fetchexclude`.
- `git lfs track` understands attribute macros such as `[attr]lfs`.
- `git lfs track` no longer rejects patterns like `**/*` in a fresh repository.
- `git lfs push --all` counts objects the server already has as done, so
  progress reaches 100%.
- `git lfs push` exits with code 2 when an upload fails, matching upstream.
- Stale temporary files under `.git/lfs/tmp` are cleaned up.
- A negative object size from the server is reported as *invalid size*.
- A credential trace line no longer appears unless `GIT_TRACE` is set.

## [0.6.0] - 2026-05-13

### Added

- SSH remotes authenticate through `git-lfs-authenticate` on the server.
- Credentials come from `.netrc`, from `GIT_ASKPASS`, `core.askpass`, or
  `SSH_ASKPASS`, and from the remote URL itself (`https://user:pass@host`).
- `credential.useHttpPath` and `credential.protectProtocol` are honored.
- `http.extraHeader` adds headers to LFS requests.
- Interrupted downloads resume where they stopped, if the server supports it.
- Batch requests and transfers are retried up to 8 times on rate limits and
  server errors, as in upstream, and a server's Retry-After header is honored.
- Uploads set a Content-Type based on the file contents. Set `lfs.contenttype`
  to false to always send `application/octet-stream`.
- `git lfs fetch --recent` also fetches recently changed refs and files,
  controlled by the `lfs.fetchrecent*` settings.
- `git lfs prune --verify-remote` only deletes objects the server still has.
  `--verify-unreachable` and `--when-unverified` control what happens to the
  rest.
- `git lfs pointer --file` applies configured LFS extensions. Use
  `--no-extensions` to skip them.
- `git lfs ext list` takes extension names to filter the list.

### Changed

- `git lfs prune` keeps objects needed by recent refs and commits, unpushed
  commits, stashes, the index, and other worktrees, matching upstream.
  `--force` and `--recent` relax these rules.
- `git lfs fetch <ref>` fetches only the files at that ref, not its history.
  Use `--all` or `--recent` for older versions.
- Error messages for authentication and fetch failures match upstream, and
  include the server's own message when it sends one.

### Fixed

- `git lfs prune`, `git lfs install`, and `git lfs env` work correctly from a
  linked worktree.
- A missing or failing askpass program no longer stops the configured
  credential helper from being tried.
- Credentials are requested again after a 403, not only after a 401.

### Documentation

- Every man page has been reviewed, and most gained examples. The
  `git-lfs-prune` and `git-lfs-fetch` man pages explain the retention and
  recent-files settings.
- The `git-lfs-config(5)` man page is a full reference of the supported
  settings.
- Installation instructions for Homebrew, apt, dnf, and `cargo install`.
- Numbered lists in man pages render correctly.

## [0.5.0] - 2026-05-03

### Added

- Release packages: binaries for Linux, macOS, and Windows (x86_64 and
  aarch64), DEB and RPM packages, and a source tarball with man pages.  The
  Linux packages are named `git-lfs-rs`, so they don't conflict with
  upstream's, but still install the `git lfs` command.
- Upstream's short flags, such as `-a` for `--all` and `-d` for `--dry-run`, on
  `fetch`, `push`, `track`, `install`, `uninstall`, `update`, `pre-push`, and
  `filter-process`.

### Changed

- Release binaries are smaller and faster.

### Fixed

- `git lfs smudge` honors `lfs.skipdownloaderrors` and
  `GIT_LFS_SKIP_DOWNLOAD_ERRORS`, and leaves the pointer in place when a
  download fails.
- `git lfs migrate info --unit` works, and accepts units without a number
  (`--unit=kb`).
- Batch responses from older servers that use `_links` instead of `actions` are
  understood.
- `git lfs ls-files` outside a repository fails with *Not in a Git repository*,
  like the other commands.
- `git lfs ls-files -- --all` suggests `--all --` instead of looking for a ref
  named `--all`.
- `git lfs ls-files --debug` output matches upstream.

### Documentation

- Man pages and `--help` text for every command follow upstream's descriptions,
  and many gained examples. The `migrate` subcommands have their own pages.
- Every man page says where to report bugs, so reports reach this project
  rather than upstream.
- `--help` output no longer shows Markdown backticks.

## [0.4.0] - 2026-05-02

### Added

- `git lfs update` installs or upgrades the Git hooks, honoring
  `core.hooksPath`. `--manual` prints instructions instead, and `--force`
  overwrites hooks you've edited.
- `git lfs install` and `git lfs uninstall` accept `--system`, `--worktree`,
  and `--file`. `git lfs uninstall hooks` removes only the hooks.
- `git lfs track --no-modify-attrs` tracks files without editing
  `.gitattributes`.
- `git lfs locks --local` lists your own locks without contacting the server.
- `git lfs fsck` accepts a commit range (`a..b`).
- LFS extensions run when files are checked out, by smudge, `git lfs pull`, and
  `git lfs checkout`.
- Client certificates, with `http.sslCert` and `http.sslKey`.
- `lfs.transfer.batchSize` is honored.
- Pushing to a remote that is a local path copies LFS objects directly.
- Without a configured remote, the LFS endpoint is taken from the last `git
  fetch`, so `git archive` works after fetching from a URL.
- After a successful Basic authentication, `lfs.<url>.access` is saved, so
  later runs send credentials straight away.
- `GIT_CURL_VERBOSE` shows batch request bodies.

### Changed

- `git lfs pull`, `git lfs fetch`, and `git lfs checkout` find LFS files
  through the index, so they respect sparse checkouts and work in bare
  repositories.
- `git lfs install` upgrades filter settings and hooks written by older
  versions, and reports conflicting hooks before changing any of them.
  `--force` also repairs duplicated filter settings.
- Leftover temporary files are cleaned up when any command starts.
- Larger objects are transferred first, as in upstream.

### Fixed

- `git lfs push` reports corrupt local objects separately from missing ones,
  and still uploads the rest.
- `git lfs push` no longer counts empty files as LFS objects.
- `git lfs push origin <ref>` works when a file has the same name as the ref.
- The pre-push hook works after a force-push that replaced commits you no
  longer have.
- The pre-push hook checks locks on lockable files that aren't LFS files.
- The pre-push hook uploads objects the server has lost, even when your
  remote-tracking ref says it has them.
- `git lfs pull` and `git lfs fetch` print nothing when there's nothing to
  fetch.
- `git lfs filter-process` honors `lfs.fetchinclude` and `lfs.fetchexclude`,
  and warns about files that should have been pointers but weren't.
- `lfs.fetchinclude` and `lfs.fetchexclude` patterns starting with `/` match
  directories.
- `git clone --reference` keeps LFS objects shared with the reference
  repository.
- `git lfs checkout --to` with `--base`, `--ours`, or `--theirs` works from a
  subdirectory.
- `git lfs track` and `git lfs untrack` honor `GIT_WORK_TREE`.
- `git lfs track` with a pattern starting with `/` marks matching files as
  modified.
- `git lfs untrack` matches patterns containing escaped characters or a leading
  `./`.
- `git lfs fsck` honors negated attribute macros such as `!lfs`, and works with
  an empty working tree when `GIT_DIR` is set.
- `git lfs lock` records the path as typed, without resolving symlinks.
- `git lfs migrate import --fixup` reads `.git/info/attributes` and
  `core.attributesFile`.
- `git lfs install` expands `~/` in `core.hooksPath`, and reports failed Git
  config changes. `git lfs uninstall` warns about them and carries on.
- `git lfs install --local`, `git lfs uninstall --local`, and `git lfs untrack`
  fail with *Not in a Git repository* outside a repository.
- An upload that gets HTTP 503 fails with *LFS is temporarily unavailable*.
- Batch responses that ask for a hash algorithm other than SHA-256 are
  rejected.

## [0.3.0] - 2026-05-01

### Added

- `git lfs migrate export` converts LFS files in history back to regular Git
  files.
- `git lfs migrate import --fixup` converts files that `.gitattributes` says
  should be LFS files but were committed without LFS.
- `git lfs migrate import --yes` skips the prompt about a dirty working tree.
- LFS extensions run when files are added. `git lfs ext` lists the configured
  extensions.
- `git lfs track --filename` treats the pattern as a literal file name.
- `git lfs checkout --to` with `--ours`, `--theirs`, or `--base` writes one
  side of a conflicted file to a path.
- `http.sslcainfo` can pin a CA certificate.
- `url.<base>.insteadOf` applies to the LFS endpoint, and to transfer URLs when
  `lfs.transfer.enablehrefrewrite` is set.
- Clones made with `git clone --shared` reuse the source's LFS objects instead
  of downloading them.

### Changed

- `git lfs env` prints the same settings as upstream.
- `git lfs status` matches upstream more closely, including renames, paths
  relative to the current directory, and bare repositories.
- `git lfs pull` and `git lfs checkout` handle conflicts and bare repositories,
  and `git lfs pull` honors `GIT_LFS_SKIP_SMUDGE`.
- `git lfs fetch --include` matches any path that refers to the same object.
- `.lfsconfig` is read from the index or HEAD when it's missing from the
  working tree. Settings that aren't safe to take from a repository are
  ignored, with a warning.
- Git config `include` directives are followed.

### Fixed

- `git lfs fetch --refetch` replaces corrupt local copies.
- `git lfs fsck` validates refs, and skips symlinks with `--pointers`.
- `git lfs track` honors `core.attributesFile`.
- The pre-push hook accepts a local path as the remote.

### Documentation

- A documentation website with installation instructions, the protocol
  specifications, and a command reference.

## [0.2.0] - 2026-04-29

Initial release: a Rust port of [Git LFS](https://github.com/git-lfs/git-lfs),
compatible with the upstream Go binary at the CLI and protocol level for the
commands below. About 58% of upstream's shell tests pass.

### Commands

- Filters and setup: `clean`, `smudge`, `filter-process`, `install`,
  `uninstall`.
- Transfers: `fetch`, `pull`, `push`, `pre-push`, and the deprecated `clone`.
- Working tree: `checkout`, `status`, `track`, `untrack`, `ls-files`.
- Locking: `lock`, `unlock`, and `locks`, with lock verification on push.
- Migration: `migrate info` and `migrate import`.
- Hooks: `pre-push`, `post-checkout`, `post-commit`, and `post-merge`,
  installed automatically by commands that need them.
- Other: `env`, `version`, `pointer`, `fsck`, `prune`.

### Networking

- HTTPS with rustls, so no system OpenSSL is needed.
- Credentials from `git credential`, cached for the session.
- The LFS endpoint is found the same way as upstream, from `GIT_LFS_URL`,
  `lfs.url`, `remote.<name>.lfsurl`, or the remote's URL.
- Parallel uploads and downloads.

[Unreleased]: https://gitlab.com/rustutils/git-lfs/-/compare/v0.8.0...HEAD
[0.8.0]: https://gitlab.com/rustutils/git-lfs/-/compare/v0.7.0...v0.8.0
[0.7.0]: https://gitlab.com/rustutils/git-lfs/-/compare/v0.6.0...v0.7.0
[0.6.0]: https://gitlab.com/rustutils/git-lfs/-/compare/v0.5.0...v0.6.0
[0.5.0]: https://gitlab.com/rustutils/git-lfs/-/compare/v0.4.0...v0.5.0
[0.4.0]: https://gitlab.com/rustutils/git-lfs/-/compare/v0.3.0...v0.4.0
[0.3.0]: https://gitlab.com/rustutils/git-lfs/-/compare/v0.2.0...v0.3.0
[0.2.0]: https://gitlab.com/rustutils/git-lfs/-/tags/v0.2.0
