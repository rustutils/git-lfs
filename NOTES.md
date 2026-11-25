# Notes

Known gaps, deferred work, and open questions, for maintainers. What has
shipped is in `CHANGELOG.md`, and per-suite test results are in
`tests/SCOREBOARD.md`.

## Path to 1.0

1.0 means full parity with upstream Git LFS:

- All of upstream's shell tests pass. Done, except `t-usage`, which checks
  upstream's usage line and stays failing on purpose.
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
  and `lfstest-testutils` are ported to Rust. `lfstest-gitserver` is ported
  too, as the `tests/server/` crate. Its server certificate is a leaf issued
  by a CA, not a self-signed CA like upstream's, because rustls rejects a
  CA as a server certificate.

## Known gaps

- **NTLM and Negotiate authentication**, and acting on `LFS-Authenticate`
  access modes. Wait for someone who needs it.

Tests that won't pass:

- `t-usage` expects upstream's usage line. Our help output comes from
  clap, so this test stays failing on purpose.

## Known bugs

Found during the docs review and the v3.8.1 test update, not fixed yet.
Where an upstream test fails because of a bug, the entry names it.
"Confirmed" means reproduced with a test or the built binary. The others
come from reading the code.

### Minor differences from upstream

- Exit codes: `fetch` with failed objects and `logs show` for a missing log
  exit 1 where upstream exits 2. `status` in a bare repository exits 1, not
  128.
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
  looks at every remote. It doesn't print "Fetching reference" for each
  ref on stderr.
- `git lfs push --object-id` doesn't find objects that exist only in an
  alternate store.
- `git lfs uninstall` only removes hooks that exactly match the current
  template.
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
- **`lfs.locksverify` accepts `yes` and `on`.** Upstream reads it with Go's
  `strconv.ParseBool`, which treats them as false, unlike its other
  booleans.

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
