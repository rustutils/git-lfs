# Porting notes

Working log for the Rust reimplementation of git-lfs: deferred items, open
questions, and milestone tracking.

## Upstream reference

The original Go implementation lives at <https://github.com/git-lfs/git-lfs>.
When behavior is ambiguous in the docs, that is the source of truth — grep
there before guessing.

Useful entry points in the upstream tree:

- `commands/` — CLI surface (one file per subcommand). Drives the `--help` UX
  we want to improve on.
- `lfs/` — pointer file format, smudge/clean filters, scanner.
- `tq/` — transfer queue (concurrent up/download with retries).
- `lfsapi/`, `lfshttp/` — batch API client + HTTP plumbing.
- `git/` — git interop (config, refs, attributes, filter-process protocol).
- `locking/` — file locks (server-side state).
- `creds/` — credential helper integration.
- `ssh/` — SSH transfer protocol.
- `fs/` — content-addressable object store on disk.
- `tools/`, `subprocess/`, `filepathfilter/` — utility layers.
- `git-lfs_windows_*.go` — Windows-only variants. Defer.

## What we vendored

- `docs/api/` — wire protocol (batch, basic transfers, locking, server discovery,
  authentication, JSON schemas). Authoritative.
- `docs/spec.md` — pointer file format. Authoritative.
- `docs/custom-transfers.md` — custom transfer agent protocol. Third-party
  contract; must match exactly.
- `docs/extensions.md` — extension protocol.
- `t/` — shell integration tests + fixtures + helpers. These drive the binary
  via its CLI, so they port for free if we keep CLI compatibility. Strongest
  safety net.

## What we deliberately skipped

- `docs/proposals/` — historical, mostly superseded.
- `docs/howto/` — user-facing docs; we'll write our own.
- `docs/man/` — generated from the upstream CLI; copying locks us into their
  `--help` output, which is what we're trying to fix.
- `docs/l10n.md` — process doc tied to upstream workflow.
- All Go source — we're rewriting, not translating.
- Go unit tests (`*_test.go`) — useful as behavioral references, but not
  portable. Reimplement alongside Rust modules.
- `t-usage.sh` — checks that the synopsis line reads `git lfs <command>
  [<args>]`. We own our help output and let clap render the default
  `Usage: git-lfs [COMMAND]`; matching the upstream wording would mean
  fighting clap on every subcommand. Stays a permanent failure.

## Remaining failure clusters

Per-suite counts live in `tests/SCOREBOARD.md`. This section is
the categorical view: what's still broken and what would unlock
it. Used to triage which milestone to pick up next.

- **Credentials family** — t-credentials, t-askpass test 4. The
  basic 401-fill-retry loop ships, but multi-attempt auth (`wwwauth[]`
  / `state[]`), per-URL `credential.<url>.helper`, and NTLM /
  Negotiate are deferred.
- **Custom transfer adapters, SSH transfer protocol, tus** —
  t-custom-transfers, t-standalone-file, t-ssh, t-batch-storage-upload-tus,
  t-multiple-remotes. Real protocol surface; basic adapter only
  ships today.
- **ls-files long tail** — `--include` / `--exclude` (needs
  filepathfilter) and the two-ref range form.
- **Migrate import** — 7 tests in t-migrate-import still fail, most
  around `--no-rewrite`, `--object-map`, and pattern-accumulation
  edge cases.
- **Unshipped commands** — `completion`, `dedup`.
- **Push edge cases** — `push (retry with expired actions)` needs
  the action-URL expiry + rebatch flow (companion to the t-expired
  cluster).
- **Single-file holdouts** — t-batch-error-handling, t-progress,
  t-batch-storage-encoding, t-batch-unknown-oids, t-clone
  (ClientCert tests).

## Highest-leverage gaps (descending leverage)

Listed by the size of the cluster they unlock. Each entry says
what's broken and where to start.

1. **Credential helper ecosystem.** The basic 401 →
   `git credential fill` → retry → approve/reject loop ships, plus
   netrc, askpass, extra HTTP headers, content-type, and
   credential-protect. Still missing: per-URL `credential.<url>.helper`
   config, stateful multi-stage auth (`state[]` / `wwwauth[]` carried
   between fills), NTLM / Negotiate. See `creds/` deferral list.
2. **Custom transfer adapters + tus + pure-SSH.** Third-party
   protocol surface; basic adapter only ships today. SSH
   `git-lfs-authenticate` ships, but the pure-SSH transfer
   protocol (`git-lfs-transfer`) doesn't.
3. **ls-files long tail.** `--include` / `--exclude` filters
   (needs filepathfilter) and the two-ref range form.
4. **Unshipped commands** — `completion`, `dedup`.
5. **Push retry-with-expired-actions.** Server hands back stale
   action URLs; client should rebatch and retry. Shares plumbing
   with the t-expired suite.

## Roadmap

Loose ordering for the deferred work. Each milestone is independent
enough to ship on its own; rough effort is small (1-3 days), medium
(1-2 weeks), large (multi-week).

### Credentials — multi-stage auth + NTLM/Negotiate

- **Per-URL credential config + multi-stage auth** —
  `credential.<url>.helper`, `state[]` / `wwwauth[]` carrying.
  Owns t-askpass test 4 plus the t-credentials tail.
- **NTLM / Negotiate** — heaviest; defer until a real Windows AD
  user surfaces.

### Custom transfer / pure-SSH / tus (large)

Three independent adapters in `transfer/`:

- **Custom transfer agent protocol** — `docs/custom-transfers.md`.
  Third-party byte-for-byte contract.
- **Tus resumable uploads** — chunk + resume + finalize.
- **Pure-SSH transfer (`git-lfs-transfer`)** — byte transfer over
  SSH instead of HTTPS. Currently we error on
  `lfs.<url>.sshtransfer=always`. `t-batch-transfer.sh` tests 6-8
  and `t-locks.sh` test 4 already pass via the basic SSH auth
  flow, but they don't actually exercise the pure-SSH protocol
  path — they happen to work because we fall through to HTTPS
  via `git-lfs-authenticate`.

### Unshipped commands (small batch)

`completion`, `dedup`. Each is small in isolation — bundle as one
focused pass.

### Long-tail polish (ongoing)

ls-files (`--include`/`--exclude`/two-ref range), push retry-with-
expired-actions, checkout `--to <path> [--ours|--theirs]`, fetch
`--recent` integration, install `--manual`, fsck `<a>..<b>` range.
Pluck individual items between bigger milestones rather than as a
single pass.

## Open questions / things to flag before deep diving

- Credential helper integration (keychain/wincred/git-credential) — what does
  the Rust ecosystem give us for free?
- Custom transfer agent protocol — third parties depend on it, must match
  byte-for-byte.
- Filter-process protocol with git itself — packet-line format, careful with
  framing.
- Concurrent transfer queue — defaults are CPU-scaled in upstream
  (commit `aa08c37f`). Worth understanding their tuning before picking ours.

## Deferred items (revisit before parity)

Things we built minimally and need to come back to. Each entry says **what's
missing** and **why it was OK to skip for v0**.

### `store`
- **Real crash-log integration.** `git lfs logs` reads/writes
  `<lfs>/logs/` correctly (lands `t-logs.sh`), but no other command
  actually emits a log on push/fetch failure yet. Wire `Panic`-style
  log writing into the fetch/push error paths so users hitting
  intermittent server errors get a postmortem to share.
- **Path encoding/decoding.** Git escapes non-ASCII paths (octal `\NNN`
  sequences) when emitting. Belongs in `git/` not `store/` — the working-
  tree path layer.

### `filter`
- **Size-mismatch cleanup.** When smudge sees an object on disk with the
  right OID but wrong size, it treats it as missing and re-fetches; we
  should also remove the corrupt local file before fetching.
- **Smudge `--` path argument.** Clean already wires the path through to
  `%f` substitution; smudge accepts it (`git-lfs smudge -- foo.bin`) but
  doesn't use it. Upstream uses it for progress/log messages and to stat
  the file for size.

### `cli` smudge / filter-process fetcher
- **`lfs.url` discovery.** `LfsFetcher` only reads `lfs.url` from the local
  scope. Upstream also reads `.lfsconfig` at the repo root and falls back
  to deriving the LFS URL from `remote.<name>.url` (server-discovery doc).
  Wire those once we have a callsite that needs them.
- **Auth.** Fetcher passes `Auth::None` — anonymous only. Real auth needs
  `creds/` (git-credential bridge) wired in. Until then, only public LFS
  endpoints work for on-demand smudge.
- **Multi-object download batching.** Each smudge that misses triggers a
  one-object batch. The filter-process protocol's `delay` capability would
  let us defer multiple smudges, batch the downloads, then return — big
  checkout speedup. Already on the deferred list under `filter-process`.

### `git`
- **`commitsOnly` scan mode** (upstream's `ScanRefRangeByTree`). Walks
  trees per commit instead of letting rev-list's `--objects` flatten the
  graph; visits the same blob multiple times but in a tree context. Used
  by upstream's `ls-files`-style commands.
- **`--recent` semantics** (upstream's `fetchRecent` /
  `lfs.fetchrecentrefsdays` / `lfs.fetchrecentcommitsdays`). Walks recent
  refs + recent commits on each ref. Layered on top of `scan_pointers`,
  not a change to it.
- **Unified rev-walk filter object** (mode + skip-deleted-blobs +
  skipped-refs). Upstream's `ScanRefsOptions` carries several flags;
  v0 only exposes plain include/exclude. Add fields as commands need them.

### `transfer`
- **Tus, custom, ssh transfer adapters.** Basic only for v0. Tus is
  upload-only (resumable PUT chunks); custom is the third-party plugin
  protocol (`docs/custom-transfers.md`); ssh is the
  `git-lfs-transfer` over SSH protocol. Each is a separate adapter file
  alongside `basic.rs`.
- **Range requests / resume.** A failed download starts over from byte 0.
  HTTP `Range:` resume needs the partial tempfile to survive across
  attempts and the server to advertise `Accept-Ranges`. Big-file users
  will care; small/typical users won't.
- **Concurrency auto-tuning.** Upstream picks `concurrency` from CPU count
  (commit `aa08c37f`); we hard-code 8. Revisit when we have benchmarks.
- **Smarter retry classification.** `is_retryable` on `TransferError::Http`
  treats anything that's not a decode/builder error as retryable. We
  could be more precise (e.g. don't retry obvious DNS failures). Punt
  until we see real failure modes.
- **Per-attempt jitter.** Backoff is pure `min(prev*2, max)`; no jitter
  to spread thundering herds. Add when we have many concurrent clients.
- **Cancellation.** No way for a caller to cancel an in-flight batch
  short of dropping the future. Add a `CancellationToken` once a CLI
  command has a Ctrl-C handler.
- **Single-object download helper.** `smudge` on a missing object will
  want to download exactly one OID without going through the batch-list
  API. Trivial wrapper over `download(vec![spec])`; add when filter
  wires up to transfer.

### `api`
- **HTTP client cert (`http.sslCert` / `http.sslKey`).** The CA-pin
  path lands via `cli/src/http_client.rs` (clears `t-clone::cloneSSL`),
  but mTLS (encrypted private keys, the `cert` credential helper
  protocol) is still missing — `t-clone::clone ClientCert` (×2) is
  blocked on it.
- **`LFS-Authenticate`-driven access mode.** We surface the header on
  401s but don't act on it (e.g. promoting to NTLM/Negotiate). Basic-auth
  retry via `creds/` is implemented; everything else is deferred.
- **Multi-stage auth (`state[]`, `wwwauth[]`).** Upstream forwards these
  between credential fills for stateful helpers (e.g. token providers).
  Our retry loop is single-stage.
- **Per-storage-URL auth.** Only the batch endpoint goes through the
  retry loop. Pre-signed action URLs (S3 etc) typically don't need creds,
  but custom storage that 401s on the action would need its own pass.
- **Typed timestamps.** `Lock.locked_at` and `Action.expires_at` are
  carried as `String`. Parsing into a typed datetime needs a date crate
  (chrono / jiff / time) — defer until a caller actually needs to compare.
- **Retry / backoff.** `is_retryable()` is a hint; the `transfer/` queue
  will own the actual retry loop with jitter/backoff.
- **Tus + custom + ssh transfer adapters.** Out of scope for `api/` (it
  only models the batch negotiation). Adapters live in `transfer/`.

### `git::endpoint`
- **`remote.<name>.pushurl`.** Upstream honors a separate push URL for
  the same remote; we only read `remote.<name>.url`. Minor accuracy gap
  for users with split read/write URLs.
- **`remote.<name>.lfspushurl`.** Per-remote push-only LFS URL. Skipped.
- **`lfs.<url>.access`.** Force an access mode (basic/ntlm/negotiate) per
  endpoint. Relevant once NTLM/Negotiate land.
- **FETCH_HEAD fallback.** Upstream falls back to the remote URL in
  `.git/FETCH_HEAD` when no other source resolves. Edge case; rarely
  matters given our `origin` default.

### `creds`
- **SSH connection multiplexing / retries.** `creds::SshAuthClient`
  ships the basic spawn + cache flow but doesn't honor
  `lfs.ssh.automultiplex` (`-oControlMaster=yes -oControlPath=...` to
  re-use a single SSH connection across calls), `lfs.ssh.retries`
  (upstream retries the SSH command up to 5 times by default), or
  `lfs.activitytimeout`. We also don't fall back to HTTP Basic when
  `git-lfs-authenticate` fails — upstream does, after the retry
  budget is exhausted. `core.sshCommand` git config is also not
  honored (we read `GIT_SSH_COMMAND` / `GIT_SSH` env vars only).
  Owns t-batch-transfer tests beyond the basic auth flow once we
  start exercising connection reuse.
- **`lfs.defaulttokenttl` fallback.** Upstream falls back to this
  config value when `git-lfs-authenticate` returns no `expires_at`
  / `expires_in`. We treat "no expiry" as "never expires until
  process exit", which is fine for the MVP test but loose for
  long-running daemons.
- **NTLM / Negotiate (Kerberos).** Upstream supports both via separate
  access modes. Out of scope until a real user hits a Windows AD
  deployment.
- **URL-pattern config.** `credential.<url>.helper` /
  `credential.<url>.useHttpPath` per-host overrides — git-credential
  does half of this for us already, and our `has_credential_helper`
  honors the host-prefix form (`credential.<scheme>://<host>.helper`)
  for askpass selection. The full URL pattern matching upstream does
  (longest-prefix wins, including path) is not yet wired into
  `useHttpPath` or general per-key lookup.
- **Multi-attempt auth retry.** `Client::send_with_auth_retry_response`
  does one fill+retry per request. Upstream's `DoWithAuth` loops up
  to 3-4 times and emits `api: too many authentication attempts` when
  the budget is exhausted. Owns t-askpass test 4 plus several
  t-credentials tests. Bundle with the wwwauth / state slice — they
  share the loop machinery.
- **Path-scoped queries.** [`Query::from_url`] populates path; the
  `Client::with_use_http_path` builder now wires the global
  `credential.useHttpPath` config through. URL-scoped
  `credential.<url>.useHttpPath` overrides land with the URL-pattern
  matching above.
- **Path bytes vs UTF-8.** `Query.path` is `String`, so our percent-
  decoder maps invalid UTF-8 byte sequences to `U+FFFD`. Upstream Go
  passes raw bytes through (Go strings hold arbitrary bytes). Real-
  world LFS paths are ASCII so no current test trips this, but the
  divergence is real. Fix: change `Query.path: String` →
  `Query.path: Vec<u8>` (or `bstr::BString`) and propagate through the
  `Helper` trait + `git_helper::write_input`. Defer until the
  whole-codebase audit shakes out other non-UTF-8 path handling.
- **Approve/reject async safety.** A `git credential approve` failure is
  swallowed (best-effort). If we ever target a flaky keystore that needs
  retry, surface it.

### `cli fetch`
- **`--json` action capture for non-dry-run.** `--json` works for
  `--dry-run` (the batch runs, URLs captured, emitted as `actions`).
  For non-dry-run we currently emit transfers without action URLs —
  needs the transfer queue to surface the batch response back to the
  caller.
- **Progress events.** v0 prints a one-line summary; we already have
  `Event::Progress` flowing through `transfer/`, just need a renderer
  (e.g. `indicatif`-based bar) wired up.

### `cli pre-push`
- **End-to-end test against real `git push`.** Our e2e tests drive
  pre-push directly with hand-built stdin. Worth a separate test that
  spawns `git push` against a wiremock-backed remote to catch hook
  invocation bugs (PATH, exit codes propagating) — but real `git push`
  needs an SSH or HTTP git remote, so the setup is heavier.

### `cli push`
- **Action-URL expiry retry.** `t-push.sh::push (retry with expired
  actions)` — server hands back an `expires_at` in the past, expecting
  the client to re-batch and pick up a fresh action URL on retry.
  We currently retry but don't re-issue the batch to refresh the URL.
  Shares plumbing with the `t-expired` suite.

### `cli pull`
- **Don't read every tracked file.** `pull` currently walks every tracked
  working-tree file and tries to parse it as a pointer (skipping anything
  ≥ MAX_POINTER_SIZE). Cheap enough for v0; for huge non-LFS repos we
  could intersect with `git ls-files :(attr:filter=lfs)` or query the
  scanner's HEAD-snapshot result first.
- **Conflict / dirty working-tree handling.** v0 happily overwrites any
  pointer-shaped file it can resolve from the store. Probably want a
  guard ("file has uncommitted edits → skip with warning") once users
  start trusting this in serious workflows.

### `cli install`
- **`--system` scope.** Trivial — just another `ConfigScope` variant.
- **`--worktree` scope.** Requires git ≥ 2.20 and worktree-feature config.
- **`--file <path>`.** Write to an arbitrary config file.
- **`--manual`.** Print instructions instead of installing.
- **`--skip-smudge`.** Different filter set (smudge gets `--skip` flag, so
  pointers stay as pointers in the working tree).
- **Upgradeable old hook contents.** Upstream tracks several historical
  hook script versions and rewrites them silently. We require exact match
  with current content (or `--force`). Migrating users from upstream Go
  will hit the conflict path; mention this once we care about that audience.

### `cli track`
- **`--filename`.** Escape glob characters in a literal filename so
  `[foo]bar.txt` matches the literal file rather than the glob.
  `t-track.sh::track: escaped glob pattern …` (×2) and the second
  invocation of `track: verbose logging` exercise it.
- **`--no-modify-attrs`.** Display-only mode that skips the
  `.gitattributes` write entirely (we already have `--dry-run`, which
  also skips the re-stage).
- **Cwd-relative pattern normalization.** When run from a subdirectory,
  upstream rewrites bare patterns relative to the repo root (so
  `cd a; git lfs track test.file` records `a/test.file`). We pass
  patterns through verbatim. `t-track.sh::track representation` covers
  this.
- **`core.attributesfile` global gitattributes** — `list_lfs_patterns`
  walks per-directory `.gitattributes` + `.git/info/attributes`, but
  doesn't read the file pointed at by `core.attributesfile`.
  `t-track.sh::track (global gitattributes)` covers this.

### Tests
- **Native `cargo test` port of the upstream `t-*.sh` suite.** The
  current setup vendors upstream's Go helpers and runs the shell tests
  via `prove`. Long-term goal: rewrite as native Rust integration
  tests so `cargo test` runs them, no `make` step, no Go toolchain.
  Big undertaking (~100 test files, ~200 assertions) — handle one
  test file at a time as we touch each command.
- **Two upstream helpers excluded** because they import internal
  upstream Go packages (`lfsapi`, `tools`, `config`):
  `lfstest-customadapter` and `lfstest-standalonecustomadapter`.
  Referenced only by `t-custom-transfers.sh` and
  `t-standalone-file.sh`; the rest of the suite doesn't need them.
  `lfstest-testutils` (the `addcommits` helper used by ~11 t-*.sh
  files for fixture-building) is reimplemented in Rust at
  `cli/src/bin/lfstest-testutils.rs`.

### `filter-process`
- **`delay` capability.** v0 handshake doesn't advertise it. Once `transfer/`
  exists, supporting delay lets us defer multiple smudges, batch the
  download, then return. Big checkout speedup; not required for correctness.
- **`list_available_blobs` command.** Pairs with `delay`.
- **`--skip` flag.** Pointer-passthrough mode for smudge (working tree keeps
  pointers literal). Useful for `git lfs install --skip-smudge` workflows.
- **Pathname-based include/exclude filter** (`lfs.fetchinclude` /
  `lfs.fetchexclude`). Lets users opt out of fetching certain large paths.
- **Malformed-pointer accumulator** + final stderr summary. Upstream prints
  a "Encountered N files that should have been pointers" report at end of
  session if any per-file `clean`/`smudge` calls hit malformed pointers.

### `cli uninstall` (deferred)
- **`--system` / `--worktree` / `--file`** — only `--global` (default) and
  `--local` wired up so far. Mirrors the install gap.
- **`uninstall hooks` subcommand** — upstream exposes hook-only removal as
  a nested subcommand. We collapse into `--skip-repo` inversion, but a
  dedicated subcommand may be worth adding for parity.

### `cli untrack`
- **`escapeAttrPattern` / `unescapeAttrPattern` parity** — upstream
  escapes `#`, spaces, and a handful of glob characters when comparing
  patterns, so `git lfs untrack 'foo bar.bin'` matches the escaped form
  written by `track`. We currently do exact-string match. Not an issue
  for typical patterns (`*.jpg`, `data/*.bin`); revisit if a test hits it.

### `cli lock` / `locks` / `unlock`
- **`locks --local` and `--cached`.** Both rely on an on-disk lock cache
  upstream maintains under `.git/lfs/cache/locks/<remote>/`; we don't
  have that cache yet. Adding it is mostly a JSON-on-disk shim around
  `Client::list_locks` results.
- **`unlock --force` path fallback.** When `resolve_lock_path` fails
  (e.g. file is gone), we currently do a minimal `\\` → `/` + strip
  `./`. Upstream canonicalizes more carefully. Revisit if tests hit it.
- **`--cached` / `--local`** for `locks` (require an on-disk lock cache
  we don't have). Tracked alongside the rest of the cache work.

### `cli ls-files`
- **`--include` / `--exclude` path filters.** Upstream filters output by
  working-tree pattern. Builds on a filepathfilter-style glob matcher
  we haven't ported yet (see also `cli fetch`).
- **Two-ref range form** — `git lfs ls-files <a> <b>` walks pointers
  added between two refs. Maps onto `rev_list(include=[b], exclude=[a])`
  but the CLI parsing must distinguish "second arg is a ref" from "second
  arg is a path".

### `cli env`
- **Trimmed output fields.** Upstream emits `LocalGitStorageDir`,
  `LocalReferenceDirs`, `ConcurrentTransfers`, `TusTransfers`,
  `BasicTransfersOnly`, `SkipDownloadErrors`, `FetchRecentAlways`,
  `FetchRecentRefsDays`, `FetchRecentCommitsDays`, `FetchRecentRemoteRefs`,
  `PruneOffsetDays`, `PruneVerifyRemoteAlways`, `PruneRemoteName`,
  `LfsExtensions`, `GitProtocol`, …. We skip these for now because most
  refer to config knobs we don't honor yet — adding stubs would lie. Add
  each as the corresponding feature lands.
- **`auth=<mode>` annotation.** Upstream prints `Endpoint=… (auth=basic)`
  / `(auth=none)` / etc. We don't track access mode per endpoint.
- **`--help` content.** Upstream's `env` is also where users go to copy a
  bug report. We could format ours as a fenced markdown block for paste-
  friendliness once the surface stabilizes.

### `cli status`
- **"Objects to be pushed to <remote>/<branch>" section.** Upstream
  prefixes its output with the LFS pointers reachable from HEAD but not
  the upstream tracking ref. Skipped for v0 because it requires resolving
  the upstream tracking ref + a separate `scan_pointers` range walk per
  invocation. Useful but not core.
- **Symlinked working dir.** Upstream resolves symlinks in `cwd` before
  computing relative paths so the displayed paths look right when the
  user `cd`'d via a symlink. We just print repo-relative paths.

### `cli migrate`
All three phases shipped: `info`, `import`, `export`. Subprocess
plumbing (fast-export → transform → fast-import + working-tree
refresh + dirty-tree refusal) lives in `migrate/pipeline.rs` so
import and export share it.

**Phase 1 deferrals (info):**
- **`--include-ref` / `--exclude-ref`.** v0 only honors positional
  branch args + `--everything`. Append-style refspec flags are a small
  follow-on; left out so the first cut keeps the CLI surface tight.
- **`--unit <unit>`.** v0 always prints with auto-scaling KB/MB/GB.
- **`--object-map`.** Records old→new commit SHAs.

**Phase 2 deferrals (import):**
- **First-commit-wins for shared blobs.** If the same blob OID appears
  at two paths with conflicting filter outcomes, the first commit's
  decision wins. Real-world impact is low (typical filters either
  match or don't match by extension) but documented for clarity.
- **In-memory blob buffering.** `--full-tree` emits every blob before
  any commit; we buffer them all in RAM until commits drain them.
  Massive repos may hit memory pressure. v2 fix: a streaming convert
  that decides without knowing the path.
- **No automatic ref backup.** We print pre-migrate ref SHAs so the
  user can roll back manually. Upstream doesn't auto-backup either.
- **`--object-map <file>`.** Same gap as info — emit old→new SHA
  mapping for downstream tooling.
- **`--verbose` per-commit progress.** v0 prints a one-line summary.
- **Working-copy-clean prompt.** v0 errors out on a dirty tree;
  upstream prompts. The friendly prompt requires TTY interaction.
- **Pattern accumulation timing.** Patterns visible to commit N
  reflect only what was discovered in commits ≤ N (matches upstream).
  An ambitious v2 could two-pass the stream so every commit's
  `.gitattributes` shows the *full* eventual pattern set.

**Phase 3 deferrals (export):**
- **Pre-download missing objects.** Upstream's `migrate export` runs
  a download queue against the configured remote first, so any
  pointer whose object isn't local gets fetched before the rewrite.
  We skip this — pointers without local content pass through
  unchanged (no truncation), and the user's expected to
  `git lfs fetch` first if they care.
- **`--remote <name>`.** Picks which remote to pre-download from.
  Tied to the deferral above.
- **Post-export `prune`.** Upstream prunes the now-orphaned LFS
  objects automatically; ours leaves them — `git lfs prune`
  manually does the job.
- **First-reference-wins.** Same caveat as import: if the same git
  blob OID lives at two paths with different filter outcomes, the
  first-encountered M directive's path decides.

### `cli post-checkout` / `post-commit` / `post-merge`
- **Diff-tree optimization.** All three hooks currently call
  `enforce_workdir`, which `git ls-files`-walks the entire index and
  chmods every lockable match. Upstream optimizes by diffing the
  before/after tree (post-checkout/post-merge) or the index (post-
  commit) and only re-stating changed paths. Worth doing once we hit a
  large-repo perf complaint; correctness is the same either way.

### `cli checkout`
- **`--to <path> [--ours|--theirs|--base]` conflict-resolution form.**
  Used during merges to extract one stage of a conflicted LFS file.
  Needs index-stage parsing (`git ls-files -s` reports stage 1/2/3 for
  conflicted entries, plus the blob shas at each stage). v0 only ships
  the bulk re-smudge mode.
- **Glob / wildcard path patterns.** v0 supports exact paths and
  trailing-slash directory prefixes only. Shells handle `*.bin` and
  `data/*.bin` for the common case (expanded against cwd before
  invocation), so the gap mostly bites recursive globs and patterns
  intended to match files that aren't in the user's cwd.
- **Progress meter.** Upstream emits a TQ-style "checking out N files"
  meter. We just print a one-line summary at the end.
- **`filepathfilter` parity.** Upstream uses gitignore-syntax matching
  (negative patterns, comments, escapes). v0's matcher is straight
  literal/prefix. When wiring this up, reach for `globset` (compile
  patterns, match strings) — `ignore` is overkill for our use case
  because we don't need its directory walker or hierarchical
  `.gitignore` traversal.

### `cli fsck`
- **`<a>..<b>` range form.** Upstream parses a single arg as either a
  ref (e.g. `HEAD`) or a range (e.g. `main..HEAD`); we only accept a
  single ref. Wire the splitter once we have a range parser worth
  reusing.
- **Index scanning when invoked bare.** With no args, upstream scans
  HEAD's history *and* the index (so newly-staged-but-uncommitted
  pointers fail fsck if their object isn't in the store). We only
  scan the named ref's history. Implementation: pair our scan with a
  `git ls-files -s` index walk.
- ~~**`unexpectedGitObject` detection.** Upstream's `--pointers` mode
  flags blobs that *should* be pointers (per `.gitattributes`) but
  don't parse.~~ Shipped — fsck loads `AttrSet::from_workdir`, walks
  every blob via `scan_tree_blobs`, and flags any LFS-tracked path
  whose blob fails to parse as a canonical pointer (or is too big).
- **`lfs.fetchexclude` honor.** Skip pointers whose paths match the
  configured exclude pattern, otherwise users who fetched a subset
  see false-positive "missing" reports.

### `cli update`
- **Hook-conflict UI.** When a custom hook exists, upstream prints
  `Hook already exists: pre-push\n\n\t<contents>\n\nTo resolve …` with
  the merge / `--force` / `--manual` advisory. We currently surface
  the install-error message inline. Owns t-update test 1.
- **Leading-space hook migration.** Upstream rewrites old templates
  whose body lines have leading TAB characters (the pre-2.6 form);
  ours treats those as a custom hook and refuses. Owns t-update
  test 2.
- **`lfs.<url>.access` migration.** Upstream rewrites `private` →
  `basic` and prunes invalid values during `update`. Tracked but no
  test currently asserts it after our 0.3 cleanups (t-update test 3
  was a no-op assertion).
- **`--manual` mode.** Print the install-by-hand instructions
  instead of writing the hook files.

### `cli pointer`
- **Compare via `git hash-object`.** Upstream computes git blob OIDs
  for both pointer texts and compares those. We compare raw byte
  equality of our canonical encoding against the supplied bytes —
  semantically identical for any real input but a small fidelity gap
  worth flagging.

### Whole-project
- **Remaining commands** — `dedup`, `standalone-file`, `update`. All
  niche; mostly polish.
