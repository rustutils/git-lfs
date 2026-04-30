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

## Suggested milestones

1. **Pointer format + clean/smudge filters.** Self-contained, no network.
   `t-clean.sh`, `t-smudge.sh`, `t-pointer.sh`, `t-malformed-pointers.sh`,
   `t-filter-process.sh` are the green-bar targets.
2. **Batch API client + basic transfer adapter.** Unlocks `fetch`/`push` for
   the happy path. `t-happy-path.sh`, `t-batch-transfer.sh`, `t-fetch.sh`,
   `t-push.sh`.
3. **Locking, custom transfers, SSH protocol, migrate** — each independent.
4. **Windows + credential helpers** — defer; flag scope before committing.

## Test status snapshot (point in time)

About 360 of 794 vendored shell tests pass (~45%) across 104
files. Notable since the last snapshot:
- **t-checkout 4/18 → 13/18** — ported pull's conflict-tolerant
  materialize (path/symlink/IsADirectory warnings, read-only
  unlink-and-restore, empty-pointer skip), plus bare-repo "must
  be run in a work tree" exit and `--git-dir` (instead of
  `--is-inside-work-tree`) for the outside-a-repo guard so
  `GIT_WORK_TREE`/`GIT_DIR` redirection works.
- **t-env 0/17 → 13/17** — full upstream line set: endpoints with
  `(auth=N)` annotations, all `Local*`/`Temp*`/`LfsStorageDir`
  paths (relative outside a repo), config-driven `Concurrent*` /
  `Tus*` / `BasicTransfersOnly` / `SkipDownload*` (with the
  `GIT_LFS_SKIP_DOWNLOAD_ERRORS` env override), custom transfer
  enumeration, sorted `GIT_*` env-var dump, canonical filter
  config defaults outside a repo. Remaining 4 fail on substantive
  features: `.lfsconfig` "unsafe key" filtering (test 8), SSH
  endpoint reporting (test 11), URL `insteadOf` alias warnings
  (test 17), and a subtle `GIT_DIR=`/`GIT_WORK_TREE=` test (9).
- **t-config 0/10 → 1/10** — only the simplest case picked up; the
  rest need `.lfsconfig`-from-HEAD-tree, URL alias resolution, and
  `.lfsconfig` unsafe-key warnings.
- **t-status 1/17 → 17/17 (full pass)** — blank-line section
  layout, cwd-relative path display, `repo_root.join` for working-
  tree reads, unstaged-then-staged ordering with first-seen-wins
  dedup, `git diff-index -M` for rename detection, "Objects to be
  pushed" section via `@{u}` resolution, missing-blob `?:
  <missing>` rendering, IsADirectory→deleted, bare-repo "must be
  run in a work tree" exit 1, empty-tree fallback before initial
  commit.
- **t-pull 1/20 → 16/20** — materialize from HEAD's tree, conflict
  warnings, `GIT_LFS_SKIP_SMUDGE`, bare-repo support, read-only
  unlink/recreate, empty-pointer skip.
- t-clone 0/13 → 8/13 and t-checkout 1/18 → 3/18 from earlier
  sessions.

The remaining failures cluster in commands we haven't started
(`env`, `config`, `ext`, `dedup`, `custom-transfers`, `ssh`,
retries) or specific feature gaps (fetch-recent windows, prune
output text, conflict-detection error messages in checkout,
locking-API edge cases).

## Release status

**v0.2.0 published to crates.io** (April 2026). All eight
workspace members have publish-ready metadata, per-crate READMEs,
and a workspace-root README that flags the experimental status.
The cli's README (which is what the crates.io listing for
`git-lfs` shows) leads with the experimental warning.

`lfstest-testutils` lives in its own non-published workspace
member at `tests/cmd/` (one `lfstest` crate; future Rust ports
of upstream test helpers drop into `tests/cmd/src/bin/<name>.rs`).
`cargo install git-lfs` installs only the production binary.

## Hook installation (corrected)

Earlier notes claimed `init.templateDir` was the key gap for fresh
clones; that turned out to be wrong. Upstream's `git lfs install`
does **not** write to `init.templateDir`, and the test framework
`testenv.sh` exports `GIT_TEMPLATE_DIR=tests/fixtures/templates`
which is hooks-empty. Hooks land in `.git/hooks/` *as a side
effect* of `installHooks(false)` calls scattered through the
upstream commands: `clean`, `smudge`, `filter-process`, `fsck`,
`track`, `untrack`, `migrate import`. Any LFS operation against a
fresh clone — even just running the smudge filter when checking
out pointer files — drops the four hook scripts.

Our Rust port now mirrors this: those six dispatch arms each call
`install::try_install_hooks(&cwd)` (best-effort, ignores errors).
Real `git lfs clone` (the deprecated wrapper) is still missing,
so t-clone's exact assertions don't pass yet, but plain
`git clone` followed by any LFS operation now leaves the working
tree in the same hook-installed state upstream produces.

## Highest-leverage gaps (descending leverage)

1. **SSH endpoint reporting**. t-env test 11 expects two-line
   endpoints: `Endpoint=…` followed by an indented
   `  SSH=user@host:path` derived from `git@host:path` style
   remote URLs.
   - **Scope**: `cli/src/env.rs::emit_endpoints`. After
     `endpoint_for_remote` resolves a URL, also keep the original
     remote URL string. If it's an SSH-shaped URL (matches
     `git@host:path`, `ssh://`, `git+ssh://`, `ssh+git://`), print
     `  SSH=<original>` on the next line, indented two spaces.
     `derive_lfs_url` already understands all the SSH forms; just
     need to expose the pre-rewrite URL alongside the post-rewrite
     one. Bonus: test 11 expects a `GIT_SSH=lfs-ssh-echo` line
     (already covered by our env-var dump when the test harness
     sets it).
2. **t-pull's remaining 4 failures** all need substantive features:
   test 11 wants `lfs.transfer.enablehrefrewrite` + git `insteadOf`
   rewrites and exit-2 on download failure; test 18 wants `git
   ls-files attr:filter=lfs` based discovery in bare repos (so an
   empty index → no fetch); test 19 needs partial-clone + sparse-
   checkout integration; test 20 needs pointer extensions.
3. **t-checkout's remaining 5 failures** are all real features:
   test 13 wants `--to <path> [--ours|--theirs|--base]` for merge
   conflict resolution (read the conflict pointer, write content
   to the target path); test 14 is a `GIT_DIR`/`GIT_WORK_TREE`
   relative-path edge case (the env vars carry over to subprocesses
   we run with `-C repo_root`, but their relative paths now resolve
   relative to repo_root rather than the original cwd; canonicalize
   on entry); test 16 is sparse checkout + partial clone (same
   `git ls-files attr:filter=lfs` discovery as t-pull 18); tests
   17 / 18 are pointer extensions.
2. **Fetch-recent semantics** (`lfs.fetchrecentrefsdays`,
   `lfs.fetchrecentcommitsdays`, `lfs.fetchrecentremoterefs`).
   Owns t-fetch-recent (1/7) and parts of t-fetch / t-prune.
3. **`git lfs env` output format**. Owns t-env (0/17). Command
   exists; output shape doesn't match what tests grep for.
4. **`git lfs config` subcommand**. Owns t-config (0/10), entirely
   unimplemented.
5. **Prune output text + `--verify-remote`**. Owns most of t-prune
   (4/18), t-prune-worktree (0/2). Output strings ("N local
   objects, M retained, done.") don't match upstream wording.
6. **Checkout conflict-detection messages**. The remaining 15
   t-checkout failures are mostly file-vs-directory and symlink
   conflict-error wording, hardlink-breaking, and empty-file mtime
   preservation.

## Other large clusters (descending leverage)

- **Custom transfer adapters + tus** — `t-custom-transfers`,
  `t-batch-storage-upload-tus`, `t-standalone-file`. Real protocol
  surface, third-party-facing.
- **Migrate `--fixup` and round-trip** — `t-migrate-fixup` (0/12),
  `t-migrate-import-no-rewrite` (0/8), and the tail in
  `t-migrate-import` (6/51) and `t-migrate-info` (7/50). Engine
  works; lots of edge cases.
- **Retry / Retry-After / rate-limit handling** —
  `t-batch-retries-ratelimit`, `t-batch-storage-retries-*` (3
  files, 0/15). Server returns 429 with Retry-After header; we
  don't honor the schedule.
- **Credential helper edge cases** — `t-credentials` (3/20),
  `t-credentials-protect`, `t-askpass` (1/6), netrc, NTLM. The
  basic 401-fill-retry loop ships; the rest of the credential
  ecosystem doesn't.
- **`status`, `ls-files` long tails** — basic forms work, exotic
  flags / formatting don't (1/17, 10/31).
- **Unshipped commands**: `ext`, `logs`, `update`, `dedup`,
  `standalone-file`, `completion`.
- **SSH transfer protocol (`git-lfs-authenticate`)** — `t-ssh`,
  parts of `t-multiple-remotes`, large parts of `t-clone`.

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
- **Alternates / reference dirs.** Needed by `t-alternates.sh` (clone with
  `--reference`). Defer until we have the config plumbing to know about
  `objects/info/alternates`.
- **Log directory** (`<lfs>/logs/`). Needed by `t-logs.sh` once we have
  commands that emit logs (push/fetch failures).
- **Permission/umask handling.** Needed by `t-umask.sh`. Tempfile defaults
  are 0600; multi-user shared repos may need 0660. Add `repo_perms` field
  on Store + `RepositoryPermissions` helper.
- **Path encoding/decoding.** Git escapes non-ASCII paths (octal `\NNN`
  sequences) when emitting. Belongs in `git/` not `store/` — the working-
  tree path layer.

### `filter`
- **Pointer extensions** (clean + smudge). `SmudgeError::ExtensionsUnsupported`
  is the current explicit refusal. Implementation = pipe content through an
  external program per extension (`docs/extensions.md`). Needed by
  `t-clean.sh` "clean with pointer extension" and `t-smudge.sh` equivalent.
- **Size-mismatch cleanup.** When smudge sees an object on disk with the
  right OID but wrong size, it treats it as missing and re-fetches; we
  should also remove the corrupt local file before fetching.
- **Working-tree path argument.** Both clean and smudge accept a path arg
  (e.g. `git-lfs clean -- foo.bin`); upstream uses it for progress/log
  messages and to stat the file for size. We currently ignore it.

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
- **SSH `git-lfs-authenticate`.** `docs/api/server-discovery.md` §SSH
  says LFS clients should run `ssh user@host git-lfs-authenticate <path>
  <op>` to get a pre-authenticated endpoint (JSON with `href` + `header`
  + `expires_in`). We currently rewrite SSH remotes to HTTPS and rely on
  the credential helper — works for GitHub/GitLab, misses self-hosted
  servers that speak only the SSH flow.
- **`remote.<name>.pushurl`.** Upstream honors a separate push URL for
  the same remote; we only read `remote.<name>.url`. Minor accuracy gap
  for users with split read/write URLs.
- **`url.<base>.insteadof` / `pushinsteadof`.** Git config aliases let
  you rewrite remote URL prefixes. Upstream applies these before endpoint
  derivation; we don't.
- **`remote.<name>.lfspushurl`.** Per-remote push-only LFS URL. Skipped.
- **`lfs.<url>.access`.** Force an access mode (basic/ntlm/negotiate) per
  endpoint. Relevant once NTLM/Negotiate land.
- **FETCH_HEAD fallback.** Upstream falls back to the remote URL in
  `.git/FETCH_HEAD` when no other source resolves. Edge case; rarely
  matters given our `origin` default.

### `creds`
- **netrc.** Upstream `creds/netrc.go` reads `~/.netrc` as a fallback.
  Skipped — `git credential` already shells through to it on most setups.
- **askpass.** `GIT_ASKPASS` / `core.askpass` for interactive password
  prompts. Niche; wire after we hear someone need it.
- **NTLM / Negotiate (Kerberos).** Upstream supports both via separate
  access modes. Out of scope until a real user hits a Windows AD
  deployment.
- **URL-pattern config.** `credential.<url>.helper` /
  `credential.<url>.useHttpPath` per-host overrides — git-credential does
  half of this for us already, but the full URL pattern matching upstream
  does is not yet wired.
- **Path-scoped queries.** [`Query::from_url`] populates path; we strip
  it via `without_path()` before querying so we match git-credential's
  default. Once URL-pattern config lands, honor `useHttpPath`.
- **Approve/reject async safety.** A `git credential approve` failure is
  swallowed (best-effort). If we ever target a flaky keystore that needs
  retry, surface it.

### `cli fetch`
- **Remote arg.** Upstream's CLI is `git lfs fetch [<remote>] [<ref>...]`;
  v0 only accepts refs. Server discovery is done — derive endpoint from
  the named remote when wiring this up.
- **`--all`.** Walk every ref in the repo (`git rev-list --all`).
- **`--recent`.** Apply `lfs.fetchrecentrefsdays` and
  `lfs.fetchrecentcommitsdays` to add recent refs + recent history.
  Big-repo polish — most common after `git fetch` to top up.
- **`--prune`.** Combine fetch with prune-after.
- **`--include`/`--exclude` patterns.** Filter pointers by working-tree
  path. Builds on top of `filepathfilter/` which we haven't ported yet.
- **`--dry-run`, `--json`, `--refetch`.** Output / behavior knobs.
- **Progress events.** v0 prints a one-line summary; we already have
  `Event::Progress` flowing through `transfer/`, just need a renderer
  (e.g. `indicatif`-based bar) wired up.

### `cli pre-push`
- **End-to-end test against real `git push`.** Our e2e tests drive
  pre-push directly with hand-built stdin. Worth a separate test that
  spawns `git push` against a wiremock-backed remote to catch hook
  invocation bugs (PATH, exit codes propagating) — but real `git push`
  needs an SSH or HTTP git remote, so the setup is heavier.
- **Push-to-remote mapping** (`url.<base>.pushInsteadOf`). Upstream's
  `git.MapRemoteURL` honors this; we use the remote name verbatim.
- **Pre-flight `verify_locks` end-to-end.** Shipped, but a couple
  of t-pre-push tests still fail because they `clone_repo` then
  `git push` without first running any LFS-side command — the
  hooks-on-smudge bootstrap (now wired through clean / smudge /
  filter-process / fsck / track / untrack / migrate-import)
  doesn't fire if no LFS path is touched between clone and push.
  A dedicated `git lfs clone` wrapper (deprecated upstream but
  still tested) would close the remaining holes.

### `cli push`
- **Batch error message format.** `t-push.sh::push with bad ref`
  greps `batch response: Expected ref "refs/heads/X", got
  "refs/heads/Y"` against the branch-required server's 403 body.
  We surface the body via `FetchError`, but format it as
  `upload failed: server returned status 403: …`. Need a custom
  formatter for batch failures.
- **Negative size in batch response.** `t-push.sh::push (with
  invalid object size)` — server returns `size: -1`. We bail at
  serde decode; upstream prints `invalid size (got: -1)`. Either
  loosen the deserializer to `i64` and validate downstream, or
  intercept the decode error.
- **Deprecated `_links` field.** `t-push.sh::push with deprecated
  _links` — old servers send `_links` instead of `actions`. Add it
  as a serde alias (or tolerant `flatten`).
- **`lfs.transfer.enablehrefrewrite` + `url.<base>.pushInsteadOf`.**
  `t-push.sh::push with invalid pushInsteadof` exercises rewriting
  the action URL via `url.<base>.pushInsteadOf` when
  `lfs.transfer.enablehrefrewrite=true`. Skipped for now.
- **Custom-namespace refs in `--all` setup.** `t-push.sh::push
  custom reference` uses `lfstest-testutils addcommits` (excluded),
  so it's gated on porting that helper.

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

### `cli fetch`
- **`--json` action capture for non-dry-run.** `--json` works for
  `--dry-run` (we run the batch, capture URLs, emit them as the
  `actions` field). For non-dry-run we currently emit transfers
  without action URLs — needs the transfer queue to surface the
  batch response back to the caller.
- **Alternates / `--shared` clone.** `t-fetch.sh::init for fetch
  tests` and `fetch (shared repository)` fail because a `--shared`
  clone has its own empty `.git/lfs/objects` while git's
  `objects/info/alternates` points at the source repo's `.git/objects`.
  Our store doesn't yet inspect alternates for LFS objects, so smudge
  fails on checkout. Fix: walk `.git/objects/info/alternates` lines,
  treat each as a fallback `<root>/lfs/objects` for store reads.
- **`--prune` integration.** Wired as a best-effort prune after the
  fetch. Upstream may have a more nuanced "fetch + prune in one
  walk" — confirm before declaring parity.
- **`Invalid remote name` for first-arg-not-a-remote.** Upstream
  treats `git lfs fetch not-a-remote` as "first arg is a remote
  name → error if not a remote" rather than "try as ref →
  Invalid ref argument". `t-fetch.sh::fetch with invalid remote`
  explicitly greps for the remote-flavor message.
- **Empty SSL key tolerance.** `t-fetch.sh::fetch does not crash on
  empty key files` sets `http.sslKey=/dev/null` and expects an
  `Error decoding PEM block` message. We don't currently surface
  that — needs a graceful path through the rustls TLS setup.

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
  working-tree pattern. Builds on the same `filepathfilter/` we haven't
  ported yet (see also `cli fetch`).
- **`--deleted`.** Include deleted-but-still-reachable LFS pointers from
  history. Pairs naturally with `scan_pointers` (which does walk history),
  but we need to surface deletions distinctly.
- **Two-ref range form** — `git lfs ls-files <a> <b>` walks pointers
  added between two refs. Maps onto `rev_list(include=[b], exclude=[a])`
  but the CLI parsing must distinguish "second arg is a ref" from "second
  arg is a path".
- **Index scan when no args.** Upstream additionally scans the index when
  invoked bare, so newly-staged-but-uncommitted pointers show up. We only
  scan the tree at HEAD.

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
- **`--fixup`.** Infer the include set from existing `.gitattributes`
  entries. Parser is now available (`git-lfs-git::attr`); the remaining
  work is reading `.gitattributes` from history rather than the working
  tree (the parser only knows about the live workdir today).
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
- **`--fixup` mode.** Parser available; needs history-aware
  `.gitattributes` loading (see info above).
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

### `cli prune`
- **Recent-refs / recent-commits retention windows.** `lfs.fetchrecentrefsdays`
  and `lfs.fetchrecentcommitsdays` keep pointers from refs / commits
  touched within those windows (plus `lfs.pruneoffsetdays` cushion). v0
  retains only HEAD's tree + unpushed; older history is fair game.
- **Worktree + stash + index walks.** Upstream also retains pointers
  reachable from other worktrees' HEADs and indexes, plus stash
  entries. We skip all three. Niche, but matters for users who lean on
  stashes or worktree-heavy workflows.
- **`--verify-remote`.** Confirm each prunable object exists on the
  remote before deleting (talks to the batch API in download-check
  mode). Needs the transfer queue's verify-only path. Useful safety net
  for users who don't fully trust their backups.
- **`--recent` / `--force`.** Inverses of "keep recent refs / keep
  unpushed." We don't have those retention paths yet, so the flags
  would be no-ops. Add when the paths exist.
- **`lfs.fetchexclude` honor.** Same gap as fsck — paths the user opted
  out of fetching shouldn't generate "missing" reports or affect
  retention.

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

### `cli pointer`
- **`--no-extensions`.** Skipped because we don't honor pointer
  extensions on the clean path either; once `lfs.extension.<n>.*`
  config support lands, build a non-extension-aware pointer when this
  flag is set.
- **Compare via `git hash-object`.** Upstream computes git blob OIDs
  for both pointer texts and compares those. We compare raw byte
  equality of our canonical encoding against the supplied bytes —
  semantically identical for any real input but a small fidelity gap
  worth flagging.

### Whole-project
- **Remaining commands** — `merge-driver`, `dedup`, `ext`,
  `standalone-file`, `logs`, `update`. All niche; mostly polish.
