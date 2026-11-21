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

The vendored files come from upstream's `main` as of late April 2026
(around `14ae1707`). The local checkout is now at `v3.8.1` (`a662a6db`).
Compare `docs/` and `tests/` against it, with `git diff 14ae1707 v3.8.1 --
docs t` in the checkout, and bring over what changed.

## Known gaps

Grouped by area, with the failing tests each one would fix.

- **Custom transfer adapters and tus.** `t-custom-transfers`,
  `t-batch-storage-upload-tus`, `t-standalone-file`, `t-multiple-remotes`.
  The protocol is in `docs/custom-transfers.md`. Two test helpers
  (`lfstest-customadapter` and `lfstest-standalonecustomadapter`) import
  upstream's internal Go packages, so they aren't built here and the tests
  that use them can't pass yet.
- **SSH URL injection.** `t-ssh`. The pure-SSH code rejects hosts starting
  with a dash (see `build_argv` in `transfer::sshtransfer::connection`), but
  `creds::ssh`, which runs `git-lfs-authenticate`, passes them through. Port
  the same check.
- **`git-lfs-authenticate` options.** We don't support `lfs.ssh.retries`,
  connection reuse with `lfs.ssh.automultiplex`, `lfs.defaulttokenttl`, or
  falling back to HTTP Basic when the command fails.
- **`core.sshCommand`** isn't read. Only `GIT_SSH_COMMAND` and `GIT_SSH`
  are.
- **NTLM and Negotiate authentication**, and acting on `LFS-Authenticate`
  access modes. Wait for someone who needs it.
- **Encrypted client certificate keys.** `t-clone` tests 3 and 4.
- **`git lfs ls-files`**: `--include` and `--exclude`, and the form that
  takes two refs. `t-ls-files` tests 13 to 15 and 25.
- **`git lfs migrate import`**: `--no-rewrite`, `--object-map`, and some
  pattern edge cases. `t-migrate-import` tests 35, 37, 44 to 47, and 49.
- **Expired actions on push.** `t-push` test 18. When an action URL has
  expired, we retry with the same URL instead of sending a new batch
  request.
- **Missing commands**: `git lfs completion` (`t-completion`) and
  `git lfs dedup` (`t-dedup`).
- **Single failing tests**: `t-batch-error-handling`, `t-progress`,
  `t-batch-storage-encoding`, `t-batch-unknown-oids`.
- **`t-usage`** expects upstream's usage line. Our help output comes from
  clap, so this test stays failing on purpose.

Gaps no test covers:

- `git lfs install --manual` (`git lfs update --manual` works).
- `git lfs locks --cached`.
- `remote.<name>.pushurl` and `remote.<name>.lfspushurl`. Pushes use the
  fetch URL.
- `git lfs update` doesn't migrate old `lfs.<url>.access` values.
- `git lfs fsck` without a ref checks HEAD but not the index.
- `git lfs migrate export` doesn't download missing objects first. Pointers
  without a local object are left as they are.
- `git lfs migrate import`: if the same blob appears at two paths and only
  one matches the filter, the first commit that uses it decides.
- `git lfs migrate info --fixup` reads `.gitattributes` from the tip commit
  only, not from each commit.

## Known bugs

Found during the docs review, not fixed yet. None has a failing test.
"Confirmed" means reproduced with a test or the built binary. The others
come from reading the code.

### Security

- **The SSH path isn't shell-quoted.** `creds/src/ssh.rs` runs
  `git-lfs-authenticate {path} {operation}` on the server with the path as
  given, and `build_argv` in `transfer/src/sshtransfer/connection.rs` does
  the same for `git-lfs-transfer`. A path like `/repo;touch x` runs extra
  commands, and a cloned repository can set it through `lfs.url` in
  `.lfsconfig`. Upstream quotes it with `ShellQuote`.
- **Redirected uploads send credentials to the new host.** Confirmed.
  `upload` in `transfer/src/basic.rs` follows redirects itself and sends
  every action header, including `Authorization`, to the new `Location`.
  Upstream drops `Authorization` and `Cookie` when the origin changes, and
  refuses HTTPS to HTTP.
- **`http.<url>.extraHeader` goes to every host.** `cli/src/http_client.rs`
  installs extra headers as client-wide defaults, so a token meant for the
  LFS server is also sent to the storage hosts in action URLs. Upstream
  looks them up per request URL. TLS settings are also endpoint-wide.
- **`http.sslVerify` typos disable TLS verification.** `parse_bool` in
  `git/src/http_options.rs` reads anything other than true, 1, yes, or on
  as false, so `http.sslVerify = ture` turns checks off. Only false, 0, no,
  and off should. `lfs_url_bool` has the same problem.
- **A password in the remote URL can appear in an error.** Confirmed. With
  `https://alice:s3cret@host/repo.git`, a 401 that no helper can answer
  gives *Git credentials for https://alice:s3cret@host/ not found*
  (`cred_url_string` in `api/src/client.rs`). `git lfs env` also prints the
  password, as upstream does.
- **`GIT_CURL_VERBOSE` prints credentials.** `http.extraHeader` values
  (`api/src/client.rs`) and transfer action headers (`transfer/src/basic.rs`)
  are printed unmasked. Upstream masks Basic credentials.
- **A pure-SSH server can crash the client.** `download` in
  `sshtransfer/adapter.rs` calls `Vec::with_capacity` with the `size=` the
  server sends, so a huge value aborts the process.

### Data loss

- **`git lfs prune --force` deletes staged objects on an unborn branch.**
  Confirmed. After `git init` and `git add a.bin`, it deletes the only
  copy. The index is only scanned when HEAD exists (`cli/src/prune.rs`).
  Upstream refuses to prune when it can't resolve the current ref.
- **`lfs.fetchinclude` makes prune delete objects HEAD needs.** Confirmed.
  Prune filters retained objects with both include and exclude. Upstream
  uses only `lfs.fetchexclude`.
- **`git lfs prune --verify-remote` asks the wrong remote.** It checks the
  default remote, not `lfs.pruneremotetocheck`.
- **Prune can try to delete `/dev/null`** when an object file named after
  the empty OID exists, since `Store::object_path` maps that OID to the null
  device. Confirmed (as a normal user it fails, and prune exits 0).
- **A global `core.hooksPath` is ignored, so pushes skip uploads.**
  Confirmed. `effective_hooks_dir` in `cli/src/install.rs` only reads the
  local scope, so hooks go to `.git/hooks` where Git never runs them. The
  automatic install in `try_install_hooks` ignores `core.hooksPath`
  entirely.
- **`git lfs migrate import --fixup` discards uncommitted changes.**
  Confirmed. It skips the dirty-tree check and then runs
  `git checkout -f HEAD`. `--no-rewrite` also skips the check. Upstream
  checks in every mode.
- **`git lfs migrate import` corrupts non-ASCII file names.** Confirmed.
  `unquote_path` in `migrate/fast_export.rs` doesn't decode octal escapes,
  so `café.bin` becomes a file named `caf\303\251.bin` in the rewritten
  history.
- **`git lfs migrate --everything` rewrites `refs/notes`.** Confirmed.
  Upstream skips stash, notes, bisect, and replace refs, and includes
  remote branches (`all_local_refs` in `migrate/mod.rs`).
- **`git lfs migrate import` converts nested `.gitattributes` files into
  pointers.** Confirmed. Only the top-level one is excluded.

### Wrong results or failures

- **`.git/info/attributes` has the lowest precedence instead of the
  highest** (`AttrSet::from_workdir` in `git/src/attr.rs`), and is assumed
  to be under the working tree, which is wrong in linked worktrees.
- **Prune fails on an unborn branch** in the main or a linked worktree, with
  *not a tree object*. Confirmed. Failed deletes print an error but exit 0.
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
- **Programs are split on whitespace.** `GIT_SSH=/opt/my tools/ssh` runs
  `/opt/my`, and quoted `GIT_SSH_COMMAND` arguments break. Same for the
  askpass program.
- **`expires_in: 0` from `git-lfs-authenticate` means expired at once**,
  so ssh runs before every request. Confirmed. Upstream treats 0 as no
  expiry.
- **Boolean environment variables and settings.** `GIT_LFS_SKIP_PUSH=false`
  skips the push, and `GIT_LFS_SKIP_SMUDGE=no` skips smudging. `get_bool`
  in `git/src/fetch_prune.rs`, `lfs.allowincompletepush`, and
  `lfs.locksverify` each parse booleans differently from Git and upstream.
- **A relative `core.hooksPath` in a bare repository** resolves outside it.
  Confirmed.
- **`git lfs uninstall` stops at the first config error** and leaves the
  hooks in place, exiting 0.
- **`git lfs clone`:** a failed pull in a submodule exits 0, `-bmain` is
  read as `-n` and skips the pull, and `--bare` skips the fetch and hooks.
- **Pre-push to a local path** ignores `--dry-run` and skips objects
  missing locally instead of failing.
- **`git lfs ls-files` lists empty files.** Confirmed.
- **`git lfs track`** prints *Touching* without `--verbose`, and picks line
  endings in a different order from upstream.
- **`create_lock` drops `Retry-After`** on rate-limit and server errors.
- **`http.sslCert` and `http.sslKey` only work with `http.sslCAInfo`.**

### Minor differences from upstream

- Exit codes: `fetch` with failed objects, `logs show` for a missing log,
  and the `ls-files` argument errors exit 1 where upstream exits 2. `status`
  in a bare repository exits 1, not 128. `update --force --manual` isn't
  rejected.
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
- `lfs.ssh.automultiplex` and the SSH variant settings aren't read, so the
  PuTTY, TortoisePlink, and simple code paths in `build_argv` are unused.
- IPv6: SSH URLs split at the wrong colon, `ssh://host:/path` gives an
  empty port, and netrc keeps the brackets.
- `read_fetch_head_url` reads the first line containing " of ", not just
  the first line.
- `git cat-file` stderr is never read, so lots of output could block.
- Memory: `git lfs filter-process` and standalone `git lfs smudge` read
  each file fully into memory, and pure-SSH downloads are buffered.
- `git lfs pointer` always computes SHA-1 blob IDs, even in SHA-256
  repositories.
- `git lfs fetch`: `--all` applies `lfs.fetchinclude`, conflicting flags
  such as `--all --recent` are accepted, and `--recent` without a remote
  looks at every remote.
- `git lfs push --object-id` doesn't find objects that exist only in an
  alternate store.
- `git lfs uninstall` prints different messages, and only removes hooks
  that exactly match the current template.
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

## Deferred improvements

- **Batched smudge downloads.** Supporting the filter protocol's `delay`
  capability would let checkout download many objects in one batch instead
  of one at a time.
- **Faster hooks.** The post-checkout, post-commit, and post-merge hooks
  walk the whole index to set lockable files read-only. Upstream only looks
  at changed paths.
- **Default concurrency.** `git lfs env` reports upstream's CPU-scaled
  default, but transfers still default to 8.
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
