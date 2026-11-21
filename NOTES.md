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
