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
- **`--dry-run`.** Upstream supports `-d` to compute what would upload
  without actually doing it. Useful for diagnostics; trivial to wire.
- **End-to-end test against real `git push`.** Our e2e tests drive
  pre-push directly with hand-built stdin. Worth a separate test that
  spawns `git push` against a wiremock-backed remote to catch hook
  invocation bugs (PATH, exit codes propagating) — but real `git push`
  needs an SSH or HTTP git remote, so the setup is heavier.
- **Push-to-remote mapping** (`url.<base>.pushInsteadOf`). Upstream's
  `git.MapRemoteURL` honors this; we use the remote name verbatim.

### `cli push`
- **`--all`.** Push every ref in the repo.
- **`--object-id <oid>`.** Upload a specific object regardless of refs.
- **`--dry-run`.** Print what would upload without doing it.
- **Local-only objects warning policy.** v0 warns and skips pointers
  whose bytes aren't in the local store. Upstream errors hard. We may
  want to expose a flag for either behavior.

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
- **Three upstream helpers excluded** because they import internal
  upstream Go packages (`lfsapi`, `tools`, `config`):
  `lfstest-customadapter`, `lfstest-standalonecustomadapter`,
  `lfstest-testutils`. The first two are referenced only by
  `t-custom-transfers.sh`; `lfstest-testutils` is build-tag-gated
  (`//go:build testtools`) so upstream's default build skips it too,
  but if a test does need it we'd have to vendor the upstream
  packages or rewrite the helper.

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
- **`unlock`'s "abort if file modified, unless --force" guard.** Upstream
  refuses to unlock a path with uncommitted edits (sane safety net so
  you don't lose collaborator-protected work). We skip the check
  entirely for v0; users see no warning. Implementation = run
  `git status --porcelain -- <path>` and refuse if non-empty.
- **`unlock --force` path fallback.** When `resolve_lock_path` fails
  (e.g. file is gone), we currently do a minimal `\\` → `/` + strip
  `./`. Upstream canonicalizes more carefully. Revisit if tests hit it.
- **Auth retry through `create_lock` / `delete_lock`.** Already noted
  under `api`: those two methods bypass the 401 → fill → retry loop in
  `Client::send_with_auth_retry`. Threading them through is a
  refactor in `api/`, not a CLI fix.

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
- **`t-post-checkout.sh` and `t-post-merge.sh`** depend on the excluded
  `lfstest-testutils` helper (`addcommits` with
  `GIT_LFS_SET_LOCKABLE_READONLY=0`), so they can't run end-to-end
  here even with full lockable support — same skip rationale as the
  other three excluded helpers.

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
