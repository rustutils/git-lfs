# Notes

Known gaps, deferred work, and open questions, for maintainers. What has
shipped is in `CHANGELOG.md`, and per-suite test results are in
`tests/SCOREBOARD.md`.

## Path to 1.0

1.0 means full parity with upstream Git LFS:

- All of upstream's shell tests pass.
- Complete documentation, `--help` output, and man pages.
- Binary releases for Linux, macOS, and Windows (done since 0.5).
- An audit of our own code's style and design.
- A comparison against upstream, to find differences the tests don't catch.

Open question: should the library crates keep sharing the binary's version
number, and how stable does their API need to be? Decide before 1.0.

## Upstream

Upstream's Go code (<https://github.com/git-lfs/git-lfs>) is the reference
when the docs are ambiguous. Useful places to look:

- `commands/`: one file per command.
- `lfs/`: pointers, filters, and the scanner.
- `tq/`: the transfer queue and adapters.
- `lfsapi/` and `lfshttp/`: the batch API client and HTTP handling.
- `creds/`: credential helpers.
- `ssh/`: `git-lfs-authenticate` and the pure-SSH protocol.
- `git/`: config, refs, attributes, and the filter protocol.

### What we vendored

- `docs/api/`, `docs/spec.md`, `docs/custom-transfers.md`,
  `docs/extensions.md`: the protocol and format specs.
- `docs/proposals/ssh_adapter.md`: the pure-SSH protocol. Upstream still
  calls it a proposal, but it's the only spec for the protocol that upstream
  and scutiger implement. The rest of `docs/proposals/` is historical, so we
  left it out.
- `tests/`: the shell tests, their fixtures, and the Go helpers they use.

We left out upstream's how-to guides and man pages, since we write our own.

The vendored files come from upstream's `v3.8.1` (`a662a6db`). To update,
diff upstream's `docs/` and `t/` between `v3.8.1` and the new release, and
bring over what changed. The JSON schemas in `docs/api/schemas/` are
symlinks upstream, into `tq/schemas/` and `locking/schemas/`.

Local changes to the vendored tests, which an update must keep:

- `tests/testenv.sh`: paths under `tests/` instead of `t/`, `export
  GIT_TERMINAL_PROMPT`, and `sha256sum` on Linux when `shasum` is missing.
- `tests/testhelpers.sh`: `setup_pure_ssh` uses `tests/scutiger/bin`.
- `tests/t-fetch.sh`, "fetch --json with remote and branches": the b-a
  fallback compares `fetch.json`, not `fetch-dry-run.json`. Upstream's
  version checks the wrong file. Not yet reported.
- `tests/cmd/lfstest-getlimit.go`: defines two constants itself instead of
  importing upstream's internal packages.
- `tests/cmd/`: `lfstest-customadapter`, `lfstest-standalonecustomadapter`,
  and `lfstest-testutils` are ported to Rust.

## Known gaps

Ordered by failing tests fixed per effort, from the triage after the
v3.8.1 update. Small means under an hour or so, medium a few hours, large
a feature.

- **Retries reuse the batch response** (medium, `t-push` 18 and
  `t-batch-storage-encoding` 4). A failed object is retried with the same
  action, where upstream sends a new batch request first. So an expired
  action fails instead of being renewed, and the tests that count batch
  requests see one fewer.
- **`git lfs migrate import` with a tag of a tag** (medium,
  `t-migrate-import` 35). `git fast-export` refuses it, so the migrate
  fails before rewriting anything.
- **`git-lfs-authenticate` options.** We don't support `lfs.ssh.retries`
  or `lfs.defaulttokenttl`.
- **NTLM and Negotiate authentication**, and acting on `LFS-Authenticate`
  access modes. Wait for someone who needs it.

Tests that won't pass:

- `t-usage` expects upstream's usage line. Our help output comes from
  clap, so this test stays failing on purpose.

Gaps no test covers:

- `git lfs install --manual` (`git lfs update --manual` works).
- `git lfs pull` checks out nothing when some objects fail to download.
  Upstream checks out the ones that did, then fails.
- `GIT_SSL_NO_VERIFY` isn't read, so it can't turn off certificate
  verification.
- `git lfs locks --cached`.
- `git lfs update` doesn't migrate old `lfs.<url>.access` values.
- `git lfs fsck` without a ref checks HEAD but not the index.
- `git lfs migrate info --fixup` reads `.gitattributes` from the tip commit
  only, not from each commit.

## Known bugs

Found during the docs review and the v3.8.1 test update, not fixed yet.
Where an upstream test fails because of a bug, the entry names it.
"Confirmed" means reproduced with a test or the built binary. The others
come from reading the code.

### Security

- **TLS settings apply to every host.** `cli/src/http_client.rs` reads
  `http.<url>.sslVerify`, `sslCAInfo`, `sslCert`, and `sslKey` for the LFS
  endpoint and uses them for storage hosts too. Upstream looks them up per
  request URL.

### Wrong results or failures

- **Flaky tests**: `t-migrate-export` test 2 or 15 sometimes fails with
  "Expanded 0 pointer(s)". `t-status` test 17 (permission change) sometimes
  lists `a.dat` as changed with the same OID, probably Git's racy-timestamp
  check. Neither investigated further.
- **`.git/info/attributes` has the lowest precedence instead of the
  highest** (`AttrSet::from_workdir` in `git/src/attr.rs`), and is assumed
  to be under the working tree, which is wrong in linked worktrees.
- **Prune exits 0 when deletes fail.** It prints an error for each, but
  upstream fails with *Prune failed, see errors above*.
- **`git lfs fetch --prune` skips the prune** when nothing needs
  downloading or with `--dry-run`, and ignores prune errors. Confirmed.
- **`git lfs migrate import --fixup` misses files added after the commit
  that adds `.gitattributes`, and fails on any commit with a symlink.**
  Confirmed. `--above` fails when the root `.gitattributes` is empty and
  nothing is converted.
- **`git lfs checkout` aborts when an object is missing and the file's
  directory doesn't exist.** Confirmed. It writes the pointer before
  creating the directory.
- **`git lfs filter-process` ignores `lfs.skipdownloaderrors`** and
  `GIT_LFS_SKIP_DOWNLOAD_ERRORS`, so a clone with them fails.
- **Extensions:** `%f` paths with spaces become several arguments
  (confirmed), smudge writes the last extension's output before checking
  its OID, and clean stores it before checking the exit status.
- **Lock verification doesn't stop the push** with `locksverify=true` on a
  404, a connection error, or any SSH error. Upstream aborts.
  `git lfs locks --local --verify` isn't rejected.
- **Lockable files with pure SSH.** The read-only hooks always query locks
  over HTTP, so with `sshtransfer=always` every lockable file becomes
  read-only. An empty `next_cursor` makes them loop. Unlock by path sends
  `unlock` on a download connection.
- **Pure SSH connections:** a read error returns the connection to the
  pool mid-message, so the next command on it fails. ssh's stderr is never
  read, so errors are lost and a chatty ssh could block. Ending the pool
  may kill the master before its clients.
- **SSH remotes can send two `Authorization` headers** after a 401, because
  reqwest appends the helper's credentials to the header from
  `git-lfs-authenticate`. Confirmed.
- **The askpass program is split on whitespace**, so a path with a space
  or a quoted argument breaks.
- **`expires_in: 0` from `git-lfs-authenticate` means expired at once**,
  so ssh runs before every request. Confirmed. Upstream treats 0 as no
  expiry.
- **Boolean environment variables and settings.** `GIT_LFS_SKIP_PUSH=false`
  skips the push, and `GIT_LFS_SKIP_SMUDGE=no` skips smudging. `get_bool`
  in `git/src/fetch_prune.rs`, `lfs_url_bool` in `git/src/http_options.rs`,
  `lfs.allowincompletepush`, and `lfs.locksverify` each parse booleans
  differently from Git and upstream.
- **`git lfs uninstall` stops at the first config error** and leaves the
  hooks in place, exiting 0.
- **`git lfs clone`:** a failed pull in a submodule exits 0, `-bmain` is
  read as `-n` and skips the pull, and `--bare` skips the fetch and hooks.
- **`create_lock` drops `Retry-After`** on rate-limit and server errors.
- **`http.sslCert` and `http.sslKey` only work with `http.sslCAInfo`.**

### Minor differences from upstream

- Exit codes: `fetch` with failed objects and `logs show` for a missing log
  exit 1 where upstream exits 2. `status` in a bare repository exits 1, not
  128. `update --force --manual` isn't rejected.
- Sizes: `ls-files --size` uses powers of 1024 with two decimals, and
  `migrate --above` reads `k`, `m`, and `g` as powers of 1024. Upstream
  uses powers of 1000.
- `git lfs status` shows paths relative to the repository's parent
  directories, not stripped of the common prefix.
- `git lfs env` doesn't read the fetch-recent, prune, or access settings,
  and doesn't print the include, exclude, or extension lines.
- Askpass: the trace line prints without `GIT_TRACE`, and empty usernames
  and passwords are rejected. A failed `git credential approve` isn't
  rolled back from the cache.
- IPv6: SSH URLs split at the wrong colon, `ssh://host:/path` gives an
  empty port, and netrc keeps the brackets.
- `read_fetch_head_url` reads the first line containing " of ", not just
  the first line.
- `git cat-file` stderr is never read, so lots of output could block.
- Memory: `git lfs filter-process` and standalone `git lfs smudge` read
  each file fully into memory.
- `git lfs pointer` always computes SHA-1 blob IDs, even in SHA-256
  repositories.
- `git lfs fetch`: `--all` applies `lfs.fetchinclude`, conflicting flags
  such as `--all --recent` are accepted, and `--recent` without a remote
  looks at every remote.
- `git lfs push --object-id` doesn't find objects that exist only in an
  alternate store.
- `git lfs uninstall` prints different messages, and only removes hooks
  that exactly match the current template.
- `git lfs migrate --everything` doesn't rewrite remote-tracking branches,
  which upstream does. Rewriting them would leave them out of step with the
  server until the next fetch, so this may be worth keeping.
- `git lfs migrate`: export doesn't exclude remote-tracking refs, has no
  `--yes`, and writes its exclude line without `-text`. Remote refs aren't
  fetched before the walk, so `--skip-fetch` does nothing. `--no-rewrite`
  commits everything staged.
- `git lfs track`: any value of `GIT_LFS_TRACK_NO_INSTALL_HOOKS` disables
  the hook install, one forbidden pattern stops the others, a failed touch
  of a read-only file is ignored, and `escape_attr_pattern` only escapes a
  leading `#`.
- With no remote named, credential queries use the LFS endpoint URL
  rather than origin's, and a URL with only a username sends Basic
  credentials with an empty password.

## Deliberate differences from upstream

- **Batch requests answered with 408 or 5xx are retried** with backoff,
  like object transfers. Upstream only retries a 429 and fails at once on a
  5xx from the batch API, although it retries a 5xx from storage. A 502 or
  503 from a proxy is often gone in seconds.

## Deferred improvements

- **Batched smudge downloads.** Supporting the filter protocol's `delay`
  capability would let checkout download many objects in one batch instead
  of one at a time.
- **Faster hooks.** The post-checkout, post-commit, and post-merge hooks
  walk the whole index to set lockable files read-only. Upstream only looks
  at changed paths.
- **Crash logs.** `git lfs logs` reads and manages logs, but no command
  writes one when it fails.
- **Retries.** No jitter in the backoff, and every HTTP error except decode
  errors is retried.
- **Cancellation.** There's no way to cancel a batch in flight other than
  dropping it.
- **Non-UTF-8 paths in credential queries.** `Query.path` is a `String`, so
  invalid UTF-8 is replaced. Upstream passes the bytes through.
- **Testing.** Port the shell tests to `cargo test`, so they don't need Go
  or `make`. Add an end-to-end test that runs a real `git push` through the
  pre-push hook.
