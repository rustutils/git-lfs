# Changelog

All notable changes to this project will be documented in this file.

The format is based on [Keep a Changelog](https://keepachangelog.com/en/1.1.0/),
and this project adheres to [Semantic Versioning](https://semver.org/spec/v2.0.0.html).

## [Unreleased]

### Added

- `git lfs track` now writes `lfs.repositoryformatversion = 0` to the
  local git config on first invocation, and errors with
  `Unknown repository format version: <val>` (exit 128) when an existing
  local value isn't `0`. Mirrors upstream's `verifyRepositoryVersion`
  from `commands/commands.go`; the global scope is intentionally
  ignored. Lands `t-repo-format`.
- `transfer`: basic upload adapter follows HTTP redirects on the action
  URL. reqwest can't auto-follow because the PUT body is a one-shot
  `ReaderStream`; we now detect 3xx responses, log
  `api: redirect PUT <old> to <new>` (gated on `GIT_TRACE`), re-open the
  file stream, and retry against the Location target. Capped at 10
  redirects. Lands `t-upload-redirect`.
- `transfer`: verify action now retries up to `lfs.transfer.maxverifies`
  attempts (default 3, values below 3 fall back to the default matching
  upstream's `tq/verify.go` clamp). Emits
  `tq: verify <short_oid> attempt #<n> (max: <mv>)` per attempt (gated
  on `GIT_TRACE`), plus `tq: verify err: <msg>` on each non-final
  failure. Exhausted-budget failures wrap as
  `TransferError::VerifyExhausted` so the outer queue retry treats them
  as terminal rather than re-issuing the whole upload. Lands `t-verify`
  (4/4).
- `api`: `GIT_CURL_VERBOSE` batch request dump now includes a masked
  `Authorization: Basic * * * * *` line so shell tests can confirm
  auth was attached without leaking credentials. Mirrors upstream's
  `lfshttp/verbose.go::traceHTTPDump`.

## [0.7.0] - 2026-05-13

### Added

- Object files committed into the local LFS store now respect
  `core.sharedRepository` (`group`/`true`/`1` → 0o770/0o660;
  `all`/`world`/`everybody`/`2` → 0o775/0o664; octal values such as
  `0660` → that mode; unset / `false` / `umask` → fall through to the
  process umask). Directories under `.git/lfs/` (`objects/`, `tmp/`,
  `incomplete/`) get the matching mode with read bits copied to
  execute bits. The `tempfile` crate creates files at 0o600
  unconditionally; we chmod after persisting so umask-respecting
  shells get the same mode they'd get from `git` itself. Wired through
  Clean / Smudge / FilterProcess / pull / fetch / checkout. Lands
  `t-umask` (4/4).
- `git lfs clone --recursive` (and `--recurse-submodules`) now runs
  `git submodule foreach --recursive 'git lfs pull'` after the
  top-level pull, so LFS content materializes in every nested
  submodule (git ≥ 2.9 forwards the disabled smudge filter to
  submodules, leaving them with pointer text otherwise). Lands
  `t-clone::clone with submodules`.
- `http.cookieFile` support: reqwest cookie jar populated from the
  configured Netscape-format file, attached to every transfer. Lets
  the LFS client traverse load balancers / proxies that gate access
  on a session cookie. URL-scoped overrides (`http.<url>.cookieFile`)
  win over global. Lands `t-clone::clone (HTTP server/proxy require
  cookies)`.
- Action-URL expiration check before upload/download. The transfer
  queue now compares the batch response's `expires_in` (preferred when
  non-zero) / `expires_at` against `now + 5s` and fails the object
  with `action "<rel>" expired` rather than driving an already-stale
  URL into the wire. Lands `t-expired` (6/6). The matching
  `t-push::push (retry with expired actions)` still fails because that
  test requires the upstream retry-then-rebatch flow we haven't
  implemented yet.
- `git lfs merge-driver`, the LFS-aware Git merge driver. Wired into
  Git via `[merge "lfs"] driver = git lfs merge-driver --ancestor %O
  --current %A --other %B --marker-size %L --output %A`. Each of
  `--ancestor` / `--current` / `--other` is smudged into a tempfile
  (fetching the object on demand if needed), the three plus a fresh
  `%D` tempfile are substituted into `--program` (default
  `git merge-file --stdout --marker-size=%L %A %O %B >%D`), and the
  merged result is cleaned back into a pointer at `--output`.
  Non-zero merge program exit propagates as conflicts. Lands
  `t-merge-driver` (6/6).
- `git lfs logs` (and `logs last` / `logs show <name>` / `logs clear` /
  `logs boomtown`). Manages the crash-log directory under
  `.git/lfs/logs/`; `boomtown` is the deliberate-failure self-test that
  writes a sample log and exits 2. Lands `t-logs`.
- `git lfs ls-files --deleted` walks the ref's full history (matching
  `scan_pointers`'s reachability semantics) so pointers reachable from
  prior commits but absent at HEAD still surface. Lands
  `t-ls-files::reference with --deleted`.
- `git lfs ls-files --all <ref>` now errors with `Cannot use --all with
  explicit reference` rather than silently ignoring the positional.
  Lands `t-ls-files::--all with argument(s)`.
- `url.<base>.pushInsteadOf` is honored for upload + verify action URLs
  when `lfs.transfer.enablehrefrewrite=true`. Falls back to plain
  `insteadOf` when no push-direction alias matches, so the existing
  download behavior is preserved. New `git_lfs_git::aliases::load_push_aliases`
  and `TransferConfig::upload_url_rewriter` carry the push-direction
  rewrite separately from the download rewriter. Lands `t-push::push
  with invalid pushInsteadof`.

### Fixed

- `creds`: gate the `creds: git credential <sub> (...)` trace line on
  `GIT_TRACE` so it stays silent when tracing is off. Matches upstream's
  `tracerx.Printf` behavior. Lands `t-lock::lock multiple files`, whose
  `grep -v CREDS errlog` assertion was tripping on the always-on lowercase
  line. Restores `t-lock` to 17/17.

- `git lfs smudge <path>` now honors `lfs.fetchinclude` /
  `lfs.fetchexclude`. When the path doesn't pass the filter, the
  pointer bytes pass through verbatim instead of triggering a
  download — matching `git lfs filter-process` and upstream's
  `command_smudge.go`. Lands `t-smudge::smudge include/exclude`.
- Temp-file cleanup at command startup now walks the full
  `.git/lfs/tmp/` tree rather than only `tmp/objects/`. Files
  matching `<64-hex>-...` whose object is already complete are
  removed unconditionally; other files older than an hour are pruned,
  with subdirectories younger than an hour exempted (hard-linked
  cross-repo temp files can look stale but still be in use). Mirrors
  upstream's `fs/cleanup.go`. Lands `t-tempfile`.
- `git lfs track` listing now expands `[attr]NAME` macros from
  top-level `.gitattributes`, `.git/info/attributes`, and the user
  attributes file (`core.attributesfile`, default
  `$XDG_CONFIG_HOME/git/attributes`). Patterns like `*.dat lfs` are
  recognized as LFS-tracked when `[attr]lfs filter=lfs ...` is in
  scope, so `git lfs track '*.dat'` correctly reports
  `"*.dat" already supported`. Subdirectory `[attr]NAME` declarations
  are ignored (git itself rejects them as "not allowed:
  dir/.gitattributes:N"). Lands `t-attributes` (4/4).
- `git lfs track` blocklist check now matches upstream's `git ls-files
  --ignored --cached -z -x <pattern>` + basename-prefix logic rather
  than textually globbing the pattern against `.gitattributes` etc.
  Patterns that *would* match a forbidden file but where no such file
  is currently tracked (e.g. `**/*` in a fresh repo) now go through
  cleanly. Existing rejection of `git lfs track .gitattributes`, `.git*`,
  `*` still works because those patterns hit committed `.gitattributes`.
  Lands `t-ls-files::list/stat files with escaped runes in path
  before commit` (which sets up via `git lfs track '**/*'`).
- `git lfs ls-files` (no args) now scans the index in addition to the
  tree at HEAD, so freshly-staged-but-uncommitted pointers show up.
  Falls back to the empty tree when HEAD doesn't exist yet, matching
  upstream's `git.EmptyTree()` path. The `*`/`-` "is the working-tree
  file present?" check now joins against the repo root so invocations
  from a subdirectory report the correct marker. Lands `t-ls-files`
  tests 3–5, 8–9, 27–31 (10 tests).
- `git lfs ls-files --json` now uses single-space indentation and ends
  with a trailing newline, matching upstream's
  `json.Encoder{SetIndent("", " "), Encode()}`.
- `git lfs push --all` (and any push path with locally-missing pointers
  the server already holds) now reports `Uploading LFS objects: 100%
  (N/N)` instead of `(M/N)` when `M < N` because the missing-locally
  objects are already on the remote. They count as already-successful
  in both the object count and byte total. Lands `t-push` tests 9–12
  (4 tests).
- `api`: `BatchResponse.objects[].size` deserialization now rejects
  negative values with `invalid size (got: -N)`, matching upstream's
  wording. Previously serde's default `u64` decoder bailed with a
  generic type error. Lands `t-push::push (with invalid object size)`.
- `git lfs push` exits with code 2 when any per-object upload fails
  (previously: code 1). Matches upstream's "push aborted" semantics
  and is what `t-push::push with invalid pushInsteadof` greps for.

## [0.6.0] - 2026-05-13

### Fixed

- `api`: `ApiError::Status` Display now surfaces the server's body
  `message` verbatim when present, falling back to
  `Authorization error: <url>` (401/403) only when the body is empty.
  Previously the body was dropped for 401/403, hiding messages like
  `Expected ref "refs/heads/other", got "refs/heads/main"` that
  `t-pre-push` / `t-fetch-refspec` / `t-push` / `t-credentials` all
  grep for. Lands 5 tests (3 near-misses across 3 suites, plus 2
  bonus from suites that shared the same root cause).
- `creds`: `HelperChain::fill` now skips helpers that error and
  continues to the next, matching upstream's `CredentialHelpers.Fill`
  (`creds/creds.go:502`). Previously a failed askpass program
  short-circuited the chain before `git credential` got a turn,
  so a missing `GIT_ASKPASS` would lock the user out instead of
  falling through to the configured credential helper.
- `creds`: emit `creds: failed to find GIT_ASKPASS command: <prog>`
  when the askpass executable isn't on `PATH`, and
  `creds: git credential <sub> (<proto>, <host>, <path>)` on every
  `git credential` invocation. Both match upstream's `tracerx.Printf`
  format at `creds/creds.go:284` / `:328`. Lands
  `t-credentials-no-prompt::askpass: push with bad askpass`.
- `git lfs fetch <ref>...` now scans only the HEAD-state of each
  named ref instead of walking its full history. Historical /
  deleted-from-HEAD pointers still get fetched via `--all` or
  `--recent`. Matches upstream's `fetchRef` vs `fetchRefs` split
  and is a prerequisite for the upcoming `--recent` semantics.

### Added

- `migrate info --fixup` now does the real per-tree attribute walk:
  list every blob at the selected ref, build a fresh `AttrSet` from
  that tree's `.gitattributes` files (root + nested), and count any
  non-attrs, non-symlink, non-pointer blob whose path is LFS-tracked
  per the attrs. Mirrors upstream's
  `commands/command_migrate_info.go::BlobFn` fixup branch. Lands
  `t-migrate-info` tests 37-41 (the `--fixup` cluster) → suite is
  now full pass 50/50. Per-commit attribute resolution across multi-
  commit history is deferred; the vendored fixup fixtures are all
  single-commit so the simplification doesn't affect any test today.

- `transfer`: Range-resume on interrupted downloads. The basic
  download adapter writes through `<lfs_dir>/incomplete/<oid>.part`
  and, when a prior attempt left a non-empty partial, sends
  `Range: bytes=<offset>-<size-1>` on the next attempt. Three status
  paths land:
  - 206 Partial Content → append to the partial (`xfer: server
    accepted resume download request`).
  - 416 Requested Range Not Satisfiable → delete the partial and
    recurse without `Range:` (`xfer: server rejected resume …
    re-downloading from start`).
  - 200 OK to a Range request → server ignored the header; treat
    as a fresh download (truncate + write).

  Partials whose size meets or exceeds the expected object size are
  treated as invalid (would produce `bytes=N-(N-1)`) and dropped
  before any request. `GIT_CURL_VERBOSE` now emits curl-style
  request/response headers on the storage GET so tests can grep
  `Range:`, `Content-Range:`, `206 Partial Content`, `416 Requested
  Range Not Satisfiable`. Mirrors upstream's `tq/basic_download.go`.
  Lands `t-batch-storage-retries` tests 3-5.
- `store`: `incomplete_dir()` / `incomplete_path(oid)` /
  `commit_partial(oid, path)` API for the resumable-download adapter.
  Hash mismatch error message changed to `expected OID {expected},
  got {actual}` so the upstream test suite can grep for the
  substring.
- `cli`: fetch failures now emit `error: failed to fetch some
  objects`, matching upstream's `commands/command_fetch.go::Exit`
  format. Previously emitted `one or more objects failed to
  download`.

- `transfer`: batch endpoint retries on 429 / 5xx, honoring
  `Retry-After` when the server pinned a wait time. `Transfer::run`
  now routes through a `batch_with_retry` helper that retries the
  batch the same way per-object transfers retry, emitting
  `tq: sending batch of size N` on every attempt and one
  `tq: enqueue retry #N after <secs>s for "<oid>" (size: M): <err>`
  per object in the batch on each retry — that's what
  `t-batch-retries-ratelimit.sh` greps for, since upstream's
  transfer queue routes each object through `enqueueRetry` at the
  batch layer. The `Retry-After` header now also surfaces on
  `ApiError::Status` via the new `retry_after()` accessor. Lands
  `t-batch-retries-ratelimit` (5 tests).
- `transfer`: `Retry-After` header parsing on storage-action 429 / 5xx
  responses. When the server pins a wait time we sleep for exactly
  that long instead of falling back to exponential backoff, mirroring
  upstream's `errors.NewRetriableLaterError` gate.
  `git_lfs_api::parse_retry_after` is the shared helper (delta-seconds
  only today; RFC 1123 deferred until a test forces it). Lands
  `t-batch-storage-retries-ratelimit` (5 tests).
- `transfer`: `with_retry` emits upstream-matching GIT_TRACE
  breadcrumbs per retry — `tq: retrying object <oid> after <secs>s`
  (Retry-After path) or `tq: retrying object <oid>: <err>` (exponential
  path), plus `tq: enqueue retry #N after <secs>s for "<oid>" (size: N): <err>`.
  Lands `t-batch-storage-retries` tests 1-2 (storage 5xx exponential
  retries).
- `transfer`: action-URL error format for fatal 5xx now prefixes
  `Fatal error:` to match upstream's `NewFatalError` wrap — the
  `t-batch-storage-retries` greps for the exact string. 4xx and the
  non-fatal 5xx (501/507/509) keep the existing `LFS:` prefix that
  `t-pull` / `t-push` grep on.
- `transfer`: default `max_attempts` bumped from 3 to 9 (= 8 retries),
  matching upstream's `defaultMaxRetries = 8`. Rate-limit windows
  (the test server uses 10s) outlast our previous 2-retry budget;
  the new budget covers ~25s of cumulative exponential backoff.

- `creds`: `NetrcCredentialHelper` reads `$HOME/.netrc` (or `_netrc`
  on Windows) at construction and slots into the helper chain ahead
  of the cache. Hosts covered by netrc don't have to round-trip
  through `git credential fill`. Parser is permissive — recognized
  keywords are `machine` / `default` / `login` / `password`;
  unknown tokens are silently skipped so other tools' annotations
  don't break the parse. Matches upstream's
  `creds/netrc.go::netrcCredentialHelper`, including the trace
  format (`netrc: git credential fill/approve/reject (…)` with
  Go's `%q` quoting) the shell tests grep on.
- `api::Client`: preemptive fill on subsequent requests after the
  first successful auth cycle. Re-walks the helper chain on every
  request once we've cached creds for the endpoint, so trace-emitting
  helpers (notably netrc) log a `fill` line per authenticated
  request — matches upstream's `setRequestAuth` flow under
  access=basic. Lands the two main netrc tests in `t-credentials.sh`
  (`credentials from netrc`, `credentials from netrc with unknown
  keyword`) plus one of two `t-credentials-no-prompt.sh` tests.

- `git`/`cli`: `http.extraHeader` / `http.<url>.extraHeader` (multi-
  value, longest-prefix match) are now installed as default headers on
  the reqwest client backing the LFS API and transfer adapter. Same
  knob proxies and enterprise gateways use to inject Authorization or
  bookkeeping headers without going through `git credential`. Header
  names are case-canonicalized by reqwest (matches upstream's
  `textproto.CanonicalMIMEHeaderKey`), so `AUTHORIZATION:` and
  `Authorization:` map to the same header. `GIT_CURL_VERBOSE` echoes
  the values in the request dump so `t-extra-header.sh`'s curl-style
  greps line up.
- `transfer`: basic upload adapter now sniffs the first 512 bytes of
  each object and sets `Content-Type` accordingly (matches upstream's
  `tq/basic_upload.go::setContentTypeFor`). Sniffing covers gzip
  (`1f 8b` → `application/x-gzip`) today; broader coverage extends
  the table when a new test demands it. `lfs.<url>.contenttype=false`
  (with `lfs.contenttype` fallback) skips detection and sends
  `application/octet-stream` — useful when a CDN rejects sniffed
  types. On HTTP 422 from the action upload, the adapter emits
  upstream's three-line stderr nudge pointing at the disable knob.
  Lands all of `t-extra-header.sh` (4 tests) and `t-content-type.sh`
  (3 tests).

- `cli/prune`: `--verify-remote` (`-c`) sends every prunable OID
  through a download-direction batch and refuses to delete anything
  the server can't serve back — protects against accidentally
  pruning the only remaining copy of a not-yet-replicated object.
  `--verify-unreachable` extends the check to orphan objects (those
  not reachable from any commit) too; without it, orphans pass
  through silently and are still pruned, matching upstream's
  `pruneGetVerifiedPrunableObjects` decision matrix.
  `--when-unverified={halt|continue}` controls what happens when
  some are missing — `halt` (default) refuses the prune and lists
  the OIDs; `continue` drops them from the delete set and prunes
  the rest. `--no-verify-remote` / `--no-verify-unreachable`
  override the corresponding `lfs.pruneverifyremotealways` /
  `lfs.pruneverifyunreachablealways` config keys for one
  invocation. Status line now reads `X local objects, Y retained,
  Z verified with remote, W not on remote, done.` Closes
  `t-prune.sh` tests 6 (`prune verify`) and 8 (`prune unreachable`)
  — `t-prune.sh` is now 18/18.
- `cli/fetcher`: `check_server_can_download(specs)` companion to
  the existing `check_server_has` — sends a download-direction
  batch and returns the OIDs the server admits to having. Used by
  `prune --verify-remote`; the existing upload-direction helper
  still serves push's "skip not-on-remote" gate.

- `creds`/`api`/`cli`: SSH-mediated auth via the `git-lfs-authenticate`
  command, the missing piece for SSH-only forge deployments. New
  `creds::SshAuthClient` spawns
  `ssh [-p <port>] <user>@<host> git-lfs-authenticate <path> <op>`,
  parses the JSON response (`href`, `header`, `expires_at`,
  `expires_in`), and caches per `(host, port, path, operation)` with a
  5s expiry buffer. `api::SshResolver` is the trait the API client
  calls before each request; a non-empty `href` overrides the LFS
  endpoint and `header` entries merge into the request. Trace lines
  (`exec: <argv>`, `ssh cache: …`, `ssh cache expired: …`) match
  upstream so the shell test greps line up. `lfs.<url>.sshtransfer`
  is partially honored: `never` emits the `skipping pure SSH
  protocol` trace upstream prints; `always` fails with `git-lfs-
  authenticate has been disabled by request` (we don't implement the
  pure-SSH transfer protocol yet). `SshInfo` now carries the SSH
  port so `ssh -p <port>` is threaded through to the command. URL
  paths returned by `git-lfs-authenticate` are normalized to collapse
  consecutive slashes, sidestepping a 301-redirect / POST→GET
  conversion that the reference test server (`lfs-ssh-echo`) would
  otherwise trip. Closes the `t-batch-transfer.sh` SSH test, the
  `t-locks.sh` SSH test (all three sub-cases), and three of six
  `t-expired.sh` tests (the SSH expiry trio).

- `creds`: new `AskpassHelper` runs `GIT_ASKPASS` /
  `core.askpass` / `SSH_ASKPASS` (in that priority order) to prompt
  for username + password, matching upstream's
  `AskPassCredentialHelper`. Trace lines (`creds: filling with
  GIT_ASKPASS: <argv>`) and prompt strings (`Username for "<url>"`,
  `Password for "<scheme>://<user>@<host>"`) are byte-compatible with
  upstream so existing test grep patterns line up.
- `cli/fetcher`: extracts `user:pass@` from the LFS endpoint URL into
  an initial `Auth::Basic` so URL-embedded credentials skip the
  401 → fill round-trip. Builds the credential helper chain with
  askpass slotted between cache and `git credential`, and skips
  askpass when a `credential.helper` is configured (URL-prefix
  lookup matches upstream's `urlConfig.Get`). Inherits the git
  remote URL as the credential URL when it shares scheme+host with
  the LFS endpoint, so prompts read like `Username for
  "https://host/repo"` instead of `.../repo.git/info/lfs`.
- `api`: `Client` gains `with_cred_url()` to override the URL used
  for credential prompts independently of the LFS endpoint;
  `cred_query` and `CredentialsNotFound` wording derive from it.
  `ApiError::Status` carries the request URL and renders 401/403 as
  upstream's `Authorization error: <url>` instead of
  `server returned status …`. Auth-retry now resets cached
  credentials on **403** as well as 401 so the next request fills
  fresh creds (matches upstream's per-request `getCreds` semantics).

### Changed

- `cli/locks_verify`: 401/403 from the lock-verify endpoint now
  prints upstream's full message — `(error|warning): Authentication
  error: Authorization error: <url>` — instead of the truncated
  `Authentication error: lock verification failed` we used before.
  Pre-push tests still match the outer `Authentication error`
  prefix; askpass tests pick up the inner URL-bearing
  `Authorization error: <url>`.

- Credential-helper plumbing for Milestone 6:
  - `creds`: `git credential` input is now validated before each
    `fill` / `approve` / `reject`. Newlines and null bytes are rejected
    unconditionally; carriage returns are rejected when
    `credential.protectProtocol` is on (the default). Error wording
    matches upstream's `creds.buffer` so existing test grep patterns
    pass.
  - `creds`: `Query::from_url` now percent-decodes the path so URLs
    with `%0a` / `%0d` / `%00` reach the helper as the literal byte —
    `protectProtocol` then catches them at the validation layer.
  - `api`: new `ApiError::CredentialsNotFound { url, detail }` variant
    surfaces upstream's `Git credentials for <url> not found:\n<detail>`
    wording when `git credential fill` returns no usable creds (or
    fails). `Client` honors `credential.useHttpPath` (default `false`)
    via the new `with_use_http_path()` builder.
  - `transfer`: new `TransferError::BatchResponse(Box<ApiError>)`
    variant prepends `batch response:` to API failures from the
    upload/download batch call so downstream error rendering matches
    upstream's `tq` wrapping. Retryability defers to the wrapped
    `ApiError`.
  - `cli/fetcher`: reads `credential.useHttpPath` and
    `credential.protectProtocol` from the effective config and
    threads both into the API client / `GitCredentialHelper`.
- `git lfs pointer --file=<path>` now runs the configured
  `lfs.extension.<name>.clean` chain in priority order when invoked
  from inside a repo, producing a pointer with `ext-N-<name>
  sha256:<input-oid>` lines and emitting
  `warning: Using LFS extensions, use --no-extensions for a plain
  pointer.` on stderr. Pass `--no-extensions` to suppress both the
  chain and the warning. When `--file` is compared against
  `--pointer` / `--stdin` and the pointers don't match, prints
  `note: Mismatch may be due to differing LFS extensions.` if either
  side has extension lines. Closes the clean-vs-pointer-CLI
  asymmetry; smudge-side extensions already shipped.
- `git lfs ext list [<name>...]` lists configured extensions, optionally
  filtered to a specific set of names. Bare `git lfs ext` and
  `git lfs ext list` (no names) keep their existing behavior of
  printing every configured extension.
- `git lfs fetch --recent` (and `lfs.fetchrecentalways`) now expands
  the fetch set with two extras: HEAD-state of every ref whose tip
  commit lies within `lfs.fetchrecentrefsdays` of today, and the
  pre-image of every LFS pointer modified within
  `lfs.fetchrecentcommitsdays` on each anchor ref. Honors
  `lfs.fetchrecentremoterefs` for whether remote-tracking refs
  participate. The pre-image walk uses a new `git log -G "oid sha256:" -p`
  diff-parsing scanner; the recent-refs walk uses a new
  `git for-each-ref --sort=-committerdate` helper.
- `git lfs prune` rewrites its retention model around the same
  config knobs as fetch-recent. It now retains: HEAD's tree, every
  recent ref's tree (within
  `lfs.fetchrecentrefsdays + lfs.pruneoffsetdays` of now), every
  recent pre-image (within
  `lfs.fetchrecentcommitsdays + lfs.pruneoffsetdays` of each
  anchor's tip date), and every commit reachable from any local
  branch or tag but not yet pushed (`git log --branches --tags
  --not --remotes=<remote>`). Honors `lfs.fetchexclude` and
  `lfs.fetchinclude` on the HEAD-tree, recent-ref, and pre-image
  paths; the unpushed walk runs unfiltered to match upstream.
  Adds `--force` (skip recent + HEAD-tree retention; keep unpushed),
  `--recent` (skip recent retention; keep HEAD + unpushed), and
  `--no-verify-remote` (no-op for now). Output strings now match
  upstream's `<N> local objects, <M> retained, done.` and
  `Deleting objects: 100% (k/n), done.` formats.
- Prune now also retains every LFS pointer reachable from
  `refs/stash` (and its WIP / index / untracked merge parents),
  every staged-but-uncommitted pointer in the current worktree's
  index, and every linked worktree's HEAD-state and index.
  Mirrors upstream's `pruneTaskGetRetainedStashed` /
  `pruneTaskGetRetainedIndex` / `pruneTaskGetRetainedWorktree`.

- LFS object storage and hook installation now resolve through
  `git rev-parse --git-common-dir` instead of `--absolute-git-dir`.
  In a non-worktree repo the two are identical; in a linked
  worktree the common-dir lookup returns the shared `.git/`
  rather than the per-worktree `.git/worktrees/<name>/`. This
  fixes prune from a worktree (which was looking at the wrong
  store and missing every object), `git lfs install` from a
  worktree (which was writing hooks to the per-worktree dir
  instead of the shared one), and the `LocalGitStorageDir` field
  of `git lfs env`. Mirrors upstream's
  `Configuration.LocalGitStorageDir`.

### Documentation

- Man-page sweep across every command page. Root `git-lfs(1)` gains
  EXAMPLES walking through the install → track → commit → push
  happy path. `git-lfs-prune(1)` fleshed out with DESCRIPTION /
  RECENT FILES / UNPUSHED LFS FILES / VERIFY REMOTE / DEFAULT
  REMOTE sections covering the M4 retention model. `git-lfs-fetch(1)`
  gains RECENT CHANGES with the `lfs.fetchrecent*` config keys.
  `git-lfs-pull(1)` gains EXAMPLES. The three `migrate-*`
  subcommand pages get per-mode EXAMPLES + SEE ALSO cross-
  references. `git-lfs-smudge(1)` gets SEE ALSO; `git-lfs-ext(1)`
  gets EXAMPLES. Section order across `fetch` / `pull` /
  `migrate` now matches upstream's INCLUDE AND EXCLUDE → DEFAULT
  REMOTE → DEFAULT REFS → RECENT CHANGES → EXAMPLES → SEE ALSO
  flow.
- `git-lfs-config(5)` rewritten from a 21-line stub to a 219-line
  reference: CONFIGURATION FILES (precedence + `.lfsconfig`
  lookup chain + `lfs.<url>.<key>` overrides), GENERAL /
  UPLOAD AND DOWNLOAD TRANSFER / PUSH / FETCH / PRUNE / EXTENSIONS
  / OTHER subsections covering ~27 config keys we honor, LFSCONFIG
  with the allowed-key list, EXAMPLES, and SEE ALSO. Keys we
  don't implement (custom-transfer agents, NTLM, dial/tls
  timeouts, etc.) are silently omitted.
- Installation instructions now cover all three packaging paths:
  Homebrew tap (Linux + macOS), Debian/Ubuntu apt, Fedora/RHEL
  dnf, plus the existing `cargo install`. README has the
  copy-paste-ready commands inline; `docs/install.md` adds context
  including the `git-lfs-rs` package-name caveat and the post-
  install `git lfs install` step.
- Two latent bugs in the groff converter fixed in passing: ordered
  lists rendered with bullet markers (`ListKind::Ordered` collapsed
  the start number — now emits `.IP "N." 4` and increments per item),
  and list-item-first-paragraph leaked across tight-list boundaries
  (`suppress_next_paragraph` flag now cleared on `End(Item)`).
- `cli/man/<cmd>/` directory grew supporting markdown for the
  above. Per-subcommand `ManContent` entries in `cli/src/man.rs`
  wire them into both the man-page and mdbook output.

## [0.5.0] - 2026-05-03

### Changed

- `--help` output no longer renders rustdoc backticks literally. The
  doc-comment markdown convention now produces clean terminal text
  (backticks stripped), bold inline-code in man pages, and proper
  links in the mdbook docs (`gitignore(5)`, `git-lfs-config(5)`,
  etc. resolve to git-scm.com or the corresponding internal page).
- `git-lfs-smudge(1)` gains ENVIRONMENT and KNOWN BUGS sections;
  `git-lfs-checkout(1)` gets the upstream-faithful DESCRIPTION
  (conflict mode, partial-clone / `GIT_ATTR_SOURCE` interaction,
  bare-repo behavior) and an EXAMPLES section.
- `git-lfs-fetch(1)` gets the upstream-faithful DESCRIPTION,
  per-flag wording, and dedicated DEFAULT REMOTE / DEFAULT REFS /
  INCLUDE AND EXCLUDE / EXAMPLES / SEE ALSO sections. Adds
  `-a`/`-p`/`-d`/`-j` short aliases for `--all`/`--prune`/
  `--dry-run`/`--json` to match upstream. The `--recent`
  flag and the `lfs.fetchrecent*` configuration are still
  unimplemented and the docs say so explicitly.
- `git-lfs-pull(1)` gets the upstream-faithful DESCRIPTION
  (with a short pointer to git-lfs-checkout(1) for the
  partial-clone / bare-repo behavior, since the same prose
  already lives there) plus DEFAULT REMOTE, INCLUDE AND
  EXCLUDE, and SEE ALSO sections.
- `git-lfs-push(1)` gets the upstream-faithful DESCRIPTION,
  per-flag wording (including the "behavior differs from
  `git lfs fetch --all`" warning on `--all`), and a SEE
  ALSO section. Adds `-d`/`-a`/`-o` short aliases for
  `--dry-run`/`--all`/`--object-id` to match upstream.
- `git-lfs-install(1)` and `git-lfs-uninstall(1)` get
  upstream-faithful DESCRIPTIONs and per-flag wording,
  plus SEE ALSO sections. Adds `-w` (`--worktree`) on
  both and `-s` (`--skip-smudge`) on install for parity.
  `--manual` is not yet supported on install — use
  `git lfs update --manual` instead.
- `git-lfs-track(1)` and `git-lfs-untrack(1)` get
  upstream-faithful DESCRIPTIONs, per-flag wording, and
  EXAMPLES + SEE ALSO sections. Adds `-d` (`--dry-run`)
  and `-j` (`--json`) short aliases on track for parity.
- `git-lfs-lock(1)`, `git-lfs-locks(1)`, and
  `git-lfs-unlock(1)` get upstream-faithful DESCRIPTIONs
  and per-flag wording, plus SEE ALSO sections. Our
  `--ref` (refspec) flag is documented as an extension
  over upstream's CLI on each command. `--cached` on
  `locks` is not yet implemented.
- `git-lfs-status(1)`, `git-lfs-ls-files(1)`,
  `git-lfs-prune(1)`, and `git-lfs-fsck(1)` get
  upstream-faithful DESCRIPTIONs and per-flag wording,
  plus SEE ALSO sections. Each page honestly notes
  unimplemented upstream features: `ls-files` skips
  `--include`/`--exclude`/`--deleted` and the two-ref
  diff form; `prune` skips the `--force`/`--recent`/
  `--verify-remote` family and the recent-files /
  stash / worktree retention rules; `fsck` skips the
  `<a>..<b>` range form and `lfs.fetchexclude` honor.
- `git-lfs-clean(1)`, `git-lfs-filter-process(1)`,
  `git-lfs-clone(1)`, `git-lfs-pointer(1)`,
  `git-lfs-version(1)`, `git-lfs-env(1)`,
  `git-lfs-ext(1)`, and `git-lfs-update(1)` get
  upstream-faithful descriptions and per-flag wording.
  Adds `-s` (`--skip`) on filter-process and `-m`/`-f`
  (`--manual`/`--force`) on update for parity with
  upstream's short aliases. The `git-lfs-clone(1)` page
  notes that `git lfs clone` no longer offers a
  meaningful speedup over plain `git clone` (which
  parallelizes the smudge filter on modern Git).
- `git-lfs-pre-push(1)`, `git-lfs-post-checkout(1)`,
  `git-lfs-post-commit(1)`, and `git-lfs-post-merge(1)`
  get upstream-faithful descriptions and SEE ALSO
  sections. Adds `-d` (`--dry-run`) on pre-push for
  parity. The post-* hook docstrings previously claimed
  "no-op stub"; corrected to reflect that all three
  now wire into the lockable read-only enforcement
  (the post-commit page notes our gap vs. upstream's
  HEAD-only optimization).
- `git-lfs-migrate(1)` and its three subcommands
  (`import`, `export`, `info`) get upstream-faithful
  descriptions and per-flag wording. The migrate parent
  page also gets INCLUDE AND EXCLUDE (with the
  migrate-specific glob semantics that differ from
  gitignore form), INCLUDE AND EXCLUDE REFERENCES
  (with the ASCII commit-graph diagram), EXAMPLES (8
  representative invocations across all three modes),
  and SEE ALSO sections. Man pages for commands with
  subcommands now include a SUBCOMMANDS section listing
  them.
- xtask now recurses into nested subcommands, generating
  a man page and mdbook page for each one — the migrate
  subcommands now have their own pages
  (`git-lfs-migrate-import(1)` etc.), so the SUBCOMMANDS
  cross-references on `git-lfs-migrate(1)` resolve to
  real pages instead of broken ones.
- Every man page and mdbook page now ends with a REPORTING BUGS
  section pointing at the project issue tracker and clarifying
  that this is the Rust port (so reports don't end up on the
  upstream Go project's tracker by mistake). Sourced from a
  single `cli/man/reporting_bugs.md`. The groff converter
  learned `.UR`/`.UE` for markdown links, so "issue tracker"
  renders as a clickable link in OSC-8-capable terminals and
  falls back to "issue tracker ⟨URL⟩" everywhere else
  (portable across groff and mandoc).

### Added

- Release packaging via `just package`. Cross-compiles `git-lfs` for
  linux-musl, darwin, and windows-gnullvm (x86_64 + aarch64 each)
  using cargo-zigbuild, and produces per-target tarballs (zips on
  windows) under `target/dist/`. Linux musl targets additionally
  build `.deb` and `.rpm` packages via cargo-deb / cargo-generate-rpm,
  named `git-lfs-rs` to avoid colliding with the upstream `git-lfs`
  package; the binary still installs as `/usr/bin/git-lfs` so
  `git lfs <command>` works after install. A source tarball
  (`git-lfs-X.Y.Z.tar.zst`) ships alongside, combining `git archive
  HEAD` with the generated man pages so downstream packagers can
  build without our xtask.
- Release binaries are now stripped, ThinLTO-optimized, single-codegen-
  unit, and `panic = "abort"` (workspace `[profile.release]`). Smaller
  and faster than the default release profile. Stripping happens during
  rustc rather than via cargo-deb's host-binutils strip, which was
  failing on cross-arch musl binaries in CI.
- GitLab CI pipeline (`lint → test → package → release → deploy`).
  Pushes to master run lint and test; semver-tagged commits
  additionally build all packaging artifacts and publish a GitLab
  release with notes pulled from the matching `CHANGELOG.md`
  section. Package job is also exposed as a manual button on master
  and merge requests so packaging can be verified without cutting a
  tag.

### Fixed

- `git lfs ls-files --debug` now terminates each pointer block
  with a trailing blank line, matching upstream's output and the
  vendored `t-ls-files` test expectations.
- Batch responses that use the deprecated `_links` field name
  (instead of `actions`) now deserialize correctly. Older LFS
  servers in the wild still emit this form.
- `git lfs ls-files` outside a git repository now prints `Not in
  a Git repository.` to stdout and exits 128, matching the other
  commands.
- `git lfs ls-files -- --all` now hints at the likely intended
  `git lfs ls-files --all --` instead of silently scanning HEAD
  for a ref named `--all`.
- `git lfs migrate info --unit=<unit>` now formats every row's
  byte count as a fractional count of the requested unit
  (`b`, `kb`, `mb`, `gb`, `tb`, `pb`) instead of being silently
  ignored. Bare unit suffixes (`--unit=kb`) are accepted as
  shorthand for `--unit=1kb`.
- `git lfs smudge` honors `lfs.skipdownloaderrors` /
  `GIT_LFS_SKIP_DOWNLOAD_ERRORS`. When the local store doesn't
  have an object and the fetch fails, the smudge filter now
  passes the original pointer text through to the working tree
  instead of failing the checkout.

## [0.4.0] - 2026-05-02

### Added

- `lfs.<url>.access = basic` is now persisted to local git config
  after a successful HTTP-Basic-authenticated request. `git lfs env`
  reads the cache to render `Endpoint=… (auth=basic)`, and the cred
  flow uses it to fill upfront on subsequent runs. Persisted at the
  end of `git lfs push`, `git lfs fetch`, smudge, and filter-process,
  so a fresh repo gets the cache after the first authenticated
  operation.
- Stale temp-object sweep on every command. At dispatch start, scan
  `<lfs>/tmp/objects/` for files whose leading 64-char OID prefix has
  a complete object in the store and remove them. Mirrors upstream's
  `lfs.cleanupTempFiles` startup task — without it, an interrupted
  download leaves behind `<oid>-<random>` temp files that pile up
  over time.
- LFS endpoint resolution now falls back to `.git/FETCH_HEAD` after
  the existing chain (`GIT_LFS_URL` → `lfs.url` → `remote.<n>.lfsurl`
  → derived from `remote.<n>.url`). Lets `git archive` smudge LFS
  files in a repo populated via a one-off `git fetch <url> refs/...`
  with no remote configured. Skipped when the caller pinned a remote
  name explicitly.
- `lfs.transfer.batchSize` is now honored. The transfer queue chunks
  the input list into runs of this size and issues one
  `POST /objects/batch` per chunk; default 100 (matches upstream).
  Each chunk emits `tq: sending batch of size N` under `GIT_TRACE`,
  the trace breadcrumb the upstream test suite greps for.
- `git lfs track --no-modify-attrs <pattern>` — track without writing
  `.gitattributes` (the user has hand-edited it). Still walks the
  index for files matching each pattern and bumps their mtime so
  git's stat-cache invalidates and the next `git status` shows them
  as modified — useful right after manually adding a `filter=lfs`
  line for an already-committed file.
- `git lfs checkout` (no path args) now discovers LFS pointers via
  `git ls-files :(attr:filter=lfs)` instead of walking HEAD's tree.
  Same sparse-checkout / bare-repo / partial-clone behavior as the
  recent `pull` change: out-of-cone files in a cone-mode sparse
  checkout aren't materialized, even after their objects have been
  fetched. Per-path filters and `--to`-mode conflict checkout are
  unchanged.
- `cargo xtask test [<suite>...] [--failures]` — runs upstream shell
  suites via `make` and prints a clean per-suite summary by parsing
  prove's TAP output (failing / passing / empty groups, plus totals).
  With no suite names, runs the full `t-*.sh` set under one outer
  setup/shutdown; with names (`pull push` or `t-pull.sh`) runs only
  those. `--failures` adds per-test failure descriptions under each
  failing suite. Also exposed as `just testsuite-summary`.
- `git lfs pull` and `git lfs fetch` (no ref args) now discover LFS
  pointers via `git ls-files :(attr:filter=lfs)` instead of walking
  HEAD's tree. Picks up the index's view of the repo: respects
  cone-mode sparse-checkout (out-of-cone files aren't fetched), works
  in bare repos against whatever's in the index, and sidesteps a
  rev-list traversal on partial clones.
- Client-cert mTLS via `http.sslCert` and `http.sslKey` (per-URL or
  global). Honored alongside `http.sslcainfo`'s pinned-CA verifier
  for the same TLS handshake.
- `git lfs update` — (re-)installs the four LFS git hooks
  (`pre-push`, `post-checkout`, `post-commit`, `post-merge`) for the
  current repository. Outside any git repo, prints
  `"Not in a Git repository."` and exits 128. The
  `lfs.<url>.access` config migration is still pending — tracked in
  `NOTES.md`.
- `pre-push` now supports local-path remotes (`git push ../sibling`,
  `git push .`) by copying each reachable LFS object directly into
  the target repo's `lfs/objects/` (hardlink with copy fallback).
- `git lfs install` and `git lfs uninstall` gain `--system`,
  `--worktree`, and `--file=<path>` scope flags alongside the
  existing `--local`/`--global` toggle, plus the upstream
  conflict-detection wording: "Only one of the --local, --system,
  --worktree, and --file options can be specified." emitted to
  stderr (exit 2) when more than one is passed. The new scope
  flows through to a per-flag `git config <scope> ...` call rather
  than the local/global-only `--global`/`--local` shortcut.
- `git lfs uninstall hooks` removes the four LFS git hooks and
  leaves the `filter.lfs.*` configuration untouched (the inverse
  of `--skip-repo`). `mode` is the upstream-style positional
  subcommand; the only accepted value today is `hooks`.
- `git lfs install --local` and `git lfs uninstall --local` now
  exit 128 with `Not in a Git repository.` (printed to stdout)
  when invoked outside any git repository, matching upstream's
  exit code and message. The check happens before any config or
  hook write.
- A failed `git config <scope> ...` invocation during install /
  uninstall now surfaces upstream's
  `error running 'git config <scope> ...': <stderr>` line on
  stdout. `install` exits 2 (e.g. `--local` against a chmod 500
  `.git`, or `--worktree` without `extensions.worktreeConfig`);
  `uninstall` treats the failure as a stdout warning and exits
  0 — uninstall is idempotent and a missing target shouldn't be
  fatal.
- `git lfs install` now silently upgrades previously-shipped
  `filter.lfs.{clean,smudge,process}` values to the current template
  (e.g. `git-lfs smudge %f` → `git-lfs smudge -- %f`), and treats
  toggling between the regular and `--skip-smudge` variants as
  upgradeable in either direction. A genuinely unrecognized value
  prints `the "filter.lfs.<x>" attribute should be "..." but is
  "..."` followed by `Run \`git lfs install --force\` to reset Git
  configuration.` on stdout and exits 2 — matching upstream's
  `lfs/attribute.go` wording. With `--force`, multivar config keys
  collapse via `git config --replace-all` so re-running install
  recovers from a `git config --add`-built config that previously
  errored out with `cannot overwrite multiple values`.
- `git lfs install` now prints the same upstream-format
  `Hook already exists: <hook>` block as `git lfs update` when one
  of the four LFS hooks has user-edited content. The pre-flight
  classification also moved into the hook installer itself, so a
  conflict on hook N no longer leaves hooks 1..N-1 already
  overwritten. A successful install now prints
  `Updated Git hooks.\nGit LFS initialized.` (the first line was
  previously omitted) when hooks are touched.
- Pointer-extension smudge support. The smudge filter, filter-process
  protocol, `git lfs pull`, and `git lfs checkout` (including
  `--to <path> --base|--ours|--theirs`) all now run configured
  `lfs.extension.<name>.smudge` commands in reverse priority order
  to reverse what `clean` did during commit. Each stage's output is
  hashed and verified against the OID recorded in the pointer's
  `ext-N-<name>` line; any mismatch surfaces as a typed error
  rather than silently producing wrong bytes. The extension
  subprocess runs from the work-tree root regardless of where the
  user invoked the command, so case-inverter / encryption shims
  that probe `.git/` work even from a subdirectory. Together these
  fix t-pull 20, t-checkout 17/18, t-filter-process 4, and
  t-smudge 4.
- `git lfs checkout --to <path> --base|--ours|--theirs <file>`
  invoked from a repo subdirectory now resolves `<file>` to a
  repo-root-relative path before looking up the staged blob via
  `git rev-parse :<stage>:<path>`. Mirrors upstream's
  `lfs.NewCurrentToRepoPathConverter`: relative args are joined
  against cwd, `..`/`.` segments are collapsed lexically, and a
  bare `.` from the repo root stays `.` so the upstream "can't
  resolve ref `:N:.`" error wording is preserved.
- `git lfs locks --local` lists the user's own locks from an
  on-disk JSON cache at `<lfs>/cache/locks.json` instead of
  contacting the server. Cache is populated as a side effect of
  every successful `git lfs lock` and pruned by `git lfs unlock`
  (both id-based and path-based). `--path` / `--id` / `--limit` /
  `--json` filter the cached records the same way the remote
  query does.
- `git lfs install` now expands a leading `~/` in
  `core.hooksPath` against `$HOME` so a hooks dir like
  `~/custom_hooks_dir` resolves to `$HOME/custom_hooks_dir`,
  matching upstream's `tools.ExpandPath`.
- `git lfs fsck <a>..<b>` now expands the rev-range into the
  concrete commits it names and unions every blob reachable from
  any of them (deduped by path + blob OID). Without this, the
  literal `<a>..<b>` got passed to `git ls-tree`, which errored
  out because it only takes a tree-ish.
- `git lfs fsck --pointers` now honors negated macro attributes
  like `b.dat !lfs`. The fix is in `git/src/attr.rs`: every
  `.gitattributes` buffer is intake-rewritten so each `!<macro>`
  reference expands to `!attr1 !attr2 …` (the keys the macro
  declares). gix-attributes does this for the positive form
  `<pattern> <macro>` but not for negation, so the rewrite fills
  the gap. Also covers the parallel `[attr]binary` builtin.
- `git lfs filter-process` smudge now honors
  `lfs.fetchinclude` / `lfs.fetchexclude`: paths excluded by the
  patterns get pointer-text passthrough at smudge time instead
  of triggering a download. Mirrors upstream's clone-time
  include/exclude (test 2 of t-filter-process).
- `git lfs fsck` now works under `GIT_DIR` /
  `GIT_OBJECT_DIRECTORY` overrides where the working tree is
  empty. The "outside repo" gate switched from
  `--is-inside-work-tree` (rejected this case because cwd was
  the parent of the work tree) to a plain "can we resolve a
  git dir," and the pointers-mode `.gitattributes` source moved
  from a workdir walk to the tree itself (so an empty work
  tree no longer hides the patterns the tree declares).

### Fixed

- The transfer queue now sorts each batch's objects by descending
  size before issuing `POST /objects/batch`, so larger transfers
  claim a parallel-transfer slot first and small ones fill in the
  tail. Matches upstream `tq` ordering and fixes t-batch-transfer
  test 2.
- A batch response advertising a `hash_algo` other than the spec
  default (`sha256`) now aborts the batch with `unsupported hash
  algorithm: <name>` before any per-object work runs. The server
  would otherwise be expecting OIDs computed under a different
  digest and its action URLs would be invalid.
- `GIT_CURL_VERBOSE=1` now dumps each `POST /objects/batch` request
  body to stderr — previously only meaningful for the libcurl-backed
  upstream. Shell tests grep these (e.g. `grep
  '{"operation":"upload"' push.log` in t-batch-transfer test 2).
- `git lfs untrack` now matches `.gitattributes` lines whose first
  token is escape-encoded (`file[[:space:]]with[[:space:]]spaces.\#`)
  against the user's literal pathname (`file with spaces.#`), and
  treats `./<path>` and `<path>` as the same pattern in either
  direction (file vs argument). Both sides are reduced to a canonical
  form (leading `./` stripped, `[[:space:]]` → space, `\#` → `#`,
  `\\` → `\`) before comparison. Outside any git repository, untrack
  now exits 128 with `fatal: not in a git repository` instead of
  silently doing nothing.
- `git lfs track` and `git lfs untrack` now write `.gitattributes`
  to the working-tree root when invoked with `GIT_WORK_TREE` pointing
  to a directory outside cwd. The previous "must be inside the work
  tree" check rejected this setup outright; the new code resolves the
  work tree via `git rev-parse --show-toplevel` (which honors the env
  var) and uses cwd only when it's actually inside the resolved tree
  — so `cd a; git lfs track foo` still writes to `a/.gitattributes`
  as before.
- `git lfs update` now recognizes seven previously-shipped hook
  templates (plus their leading-tab indented variants) as ours and
  silently upgrades them to the current template, prints the
  upstream-format `Hook already exists: <hook>` block when a
  user-edited hook is in the way, and supports `--manual` to print
  install instructions for all four hooks. `--force` overwrites a
  conflict; the conflict path exits non-zero without touching any
  hook. The hooks directory now honors `core.hookspath` so writes
  and the manual-mode display path follow whatever git would
  actually invoke.
- `git lfs push origin <ref>` no longer fails with
  `fatal: ambiguous argument` when a working-tree file shares its
  name with the pushed ref (e.g. `git lfs push origin main` in a
  repo that tracks a file literally named `main`). The lock-verify
  helper's `git log -z --pretty=format: --name-only <revs>` invocation
  now ends with `--`, so git treats every preceding argument as a rev
  and never attempts the rev/path disambiguation that was breaking
  the push.
- `git lfs filter-process` now emits the upstream-compatible
  `Encountered N file(s) that should have been pointer(s), but
  weren't:` warning to stderr at end-of-session, listing each
  pathname that smudge passed through unchanged because the blob
  didn't parse as a pointer. The aggregated message lets shell tests
  (and humans grepping `clone.log`) see which working-tree files
  ended up with raw content where a pointer was expected. Empty
  blobs don't count — those are legitimate empty files.
- `Store::insert` no longer rewrites the destination file when the
  resulting OID is already present locally. The store is content-
  addressed, so a repeat insert of the same bytes is necessarily a
  no-op; previously the `tmp.persist` rename would atomically swap a
  fresh inode in for the existing one, breaking any hardlink set up
  by `Store::with_references` materialization. Matters for
  `git clone --reference`: after `git lfs pull` hardlinks the
  reference repo's LFS object into the local store, the post-pull
  `git update-index --refresh` invokes the clean filter, which would
  re-insert and clobber the hardlink. The corrupt-recovery path
  (`insert_verified`, used for downloads) still overwrites
  unconditionally.
- `git push` now skips empty files committed to a `filter=lfs` path
  when counting LFS objects to upload. Git stores those as the empty
  blob (not as a pointer text), but our scanner parses empty input as
  `Pointer::empty()` (size 0), and that was inflating the
  `Uploading LFS objects: N/N` count. Filtered out at the partition
  step alongside the corrupt/missing logic.
- `git lfs track` with a root-anchored pattern (e.g. `git lfs track
  /a.dat`) now correctly mtime-bumps matching files. The post-track
  `git ls-files -- /a.dat` was erroring with "outside repository"
  (git treats a leading `/` as an absolute path in pathspecs); the
  empty result meant no `Touching` line and no stat-cache invalidation,
  so the next `git add` saw the file as unchanged and skipped the
  clean filter. Strip the leading slash before passing the pattern
  to `git ls-files`.
- `git push` now distinguishes locally-corrupt LFS objects (file
  exists on disk but its size doesn't match the pointer) from
  truly-missing ones. Corrupt objects are reported as
  `(corrupt) <path> (<oid>)` after the healthy objects upload —
  matching upstream's behavior where present-locally objects in the
  same push reach the server even though the corrupt one fails the
  overall push. Truly-missing objects keep their pre-flight gate
  governed by `lfs.allowincompletepush`; corrupt always fails the
  push regardless of that setting.
- `lfs.fetchinclude` / `lfs.fetchexclude` patterns starting with `/`
  (e.g. `/foo`) now match subtrees as upstream's filepathfilter does.
  The leading `/` is upstream's root-anchor marker; we strip it
  before compiling the glob, since `matches_with_prefix` already
  walks ancestor directories. `git lfs fsck` is the most visible
  caller — without the strip, `lfs.fetchexclude=/foo` failed to
  exclude `foo/a.dat` from the corrupt-objects scan.
- `git lfs pull` / `git lfs fetch` (no refs) no longer print
  `Downloading LFS objects: 0% (0/0)` when there's nothing to
  fetch — silent on the empty case.
- `git lfs lock <path>` no longer follows symlinks when normalizing
  the user-supplied path. `git lfs lock folder1/folder2/a.dat`
  records the path as typed, even when an intermediate component is
  a symlink to a sibling directory.
- `git lfs migrate import --fixup` now consults `.git/info/attributes`
  (highest precedence) and `core.attributesFile` (lowest), in addition
  to the per-commit `.gitattributes`, when deciding which paths should
  be LFS-tracked. Matches Git's documented attribute lookup order.
- HTTP 503 from a storage endpoint during upload is now reported as
  `LFS is temporarily unavailable` (matching upstream's wording),
  instead of the generic `Server error … from HTTP 503`.
- `pre-push` no longer errors with `fatal: bad object …` after a
  force-push whose old remote-side commit was GC'd locally — excludes
  whose OIDs aren't in the local object database are dropped before
  rev-list.
- `pre-push` lock verification now covers lockable-but-non-LFS files
  (`*.dat lockable` without `filter=lfs`). The intersection set is
  every path changed in the push range, not just LFS pointer paths.
- `pre-push` catches LFS objects the server has GC'd while a stale
  local remote-tracking ref still points at them — a safety-net
  unrestricted rev-list pass after the optimized one routes any
  newly-discovered pointers through the missing-on-server probe.

## [0.3.0] - 2026-05-01

### Added

- `git lfs ext` — list configured pointer extensions.
- Clean-side pointer extensions: `lfs.extension.<name>.{clean,priority}`
  programs are chained over content during `git add`, with each phase's
  input OID recorded as `ext-N-<name>` in the emitted pointer (per
  [`docs/extensions.md`](docs/extensions.md)). Smudge-side support is
  still pending.
- `git lfs migrate export` — full history rewrite from LFS pointers
  back to inline blobs, with `--object-map`, `--include-ref`,
  `--exclude-ref`, `--remote`, and `--verbose`.
- `git lfs migrate --fixup` — re-runs LFS conversion against the
  current `.gitattributes`, evaluated per commit (so rules added later
  apply backwards through history).
- `git lfs migrate import --yes` — bypass the dirty-working-tree
  prompt for unattended runs.
- `git lfs track --filename` — track a path as a literal name pattern,
  escaping glob metacharacters in the emitted `.gitattributes` line.
- `git lfs checkout --to <path> --ours|--theirs|--base` — extract one
  side of a conflicted LFS blob to a path.
- TLS pinning via `http.sslcainfo`: a custom CA bundle is honored even
  when the leaf certificate is itself a CA, matching upstream's
  cert-pinning behavior.
- LFS action URLs returned by the batch API are now rewritten through
  `lfs.transfer.enablehrefrewrite` + `url.<base>.insteadof`.
- `url.<base>.insteadof` is also applied when deriving an LFS endpoint
  from `remote.<name>.url`.
- LFS objects are hardlinked (or copied on cross-device fallback) from
  a `git clone --shared` source's `lfs/objects/`, so shared clones
  don't re-download content.
- mdbook-rendered documentation site: introduction, install, vendored
  protocol/format schemas, command reference grouped by surface.
- Logo and banner branding in the README.
- [`tests/SCOREBOARD.md`](tests/SCOREBOARD.md) — per-suite snapshot of
  the vendored upstream shell tests.

### Changed

- `git lfs env` — full upstream output line set, config-driven values,
  SSH metadata reported under each `Endpoint:` line, canonicalized
  `GIT_DIR`, and empty filter values when filters are unset.
- `git lfs status` — bare-repo handling, missing-blob safety,
  file-to-dir transitions, push section, blank-line layout,
  cwd-relative paths, rename detection, deterministic ordering.
- `git lfs checkout` — bare-repo handling, conflict-tolerant
  materialize (ported from `pull`).
- `git lfs pull` — walks HEAD's tree directly, handles conflicts,
  honors `GIT_LFS_SKIP_SMUDGE`, tolerates read-only directories and
  empty pointers, runs in bare repos.
- `git lfs fetch` — `--include` patterns match any path that points to
  the same LFS OID; trailing `/` on a pattern is stripped before
  matching; tolerant of `size`-less batch responses.
- `git lfs fetch --json` captures the batch response in non-dry-run
  mode.
- `git lfs fsck` — validates refs and skips symlinks in `--pointers`
  mode.
- `git lfs track` — cwd-relative match in listings; honors
  `core.attributesFile`.
- `git lfs pre-push` — uses `git rev-list ... --not --remotes=<name>`
  for the missing-on-remote walk; validates the remote name with a
  local-path fallback.
- `.lfsconfig` is read from the index and HEAD when the working-tree
  file is missing; unsafe keys are filtered out (matches upstream's
  `safeKeys` allowlist) with a one-shot warning.
- `git config` lookups now go through `--includes` and read scope-less
  so cross-scope values resolve correctly.

### Fixed

- `git lfs fetch --refetch` now reliably overwrites corrupt local
  copies — the store clobbers existing files on commit instead of
  failing the rename.

## [0.2.0] - 2026-04-29

Initial public release on [crates.io](https://crates.io/crates/git-lfs).

This is a from-scratch Rust port of
[Git LFS](https://github.com/git-lfs/git-lfs), feature-compatible with
the upstream Go binary at the CLI and wire-protocol level for the
command surface listed below. About 446 of the ~770 vendored upstream
shell tests pass at this version (~58 %).

### Commands

- **Filters and install.** `clean`, `smudge`, `filter-process`,
  `install`/`uninstall` (local and global scopes, `--skip-smudge`,
  `--skip-repo`, `--force`).
- **Transfer.** `fetch` (with `--include`/`--exclude`/`--refetch`/
  `--all`/`--dry-run`/`--json`), `pull`, `push`
  (`--all`/`--stdin`/`--object-id`, plain-URL remotes), `pre-push`,
  `clone` (deprecated upstream wrapper).
- **Working tree.** `checkout` (glob patterns, progress meter,
  missing-object fallback), `status`, `track`, `untrack`, `ls-files`.
- **Locking.** `lock`, `unlock`, `locks`, with the full
  `lfs.<endpoint>.locksverify` matrix on `pre-push`.
- **Migration.** `migrate info`, `migrate import`,
  `migrate import --no-rewrite`.
- **Hooks.** All four entry points (`pre-push`, `post-checkout`,
  `post-commit`, `post-merge`), auto-installed by `clean`, `smudge`,
  `filter-process`, `fsck`, `track`, `untrack`, and
  `migrate import`.
- **Other.** `env`, `version`, `pointer`, `fsck`, `prune`.

### Networking

- Batch and locking HTTP client (rustls TLS, no system OpenSSL).
- 401 → `git credential fill` → retry-once → `approve`/`reject` loop
  with an in-memory credential cache.
- Endpoint resolution walks the full upstream priority chain:
  `GIT_LFS_URL` → `lfs.url` (git config and `.lfsconfig`) →
  `remote.<name>.lfsurl` → derived from `remote.<name>.url`
  (SSH/git URL → HTTPS rewriting).
- Concurrent transfer queue with the basic adapter (upload, download,
  verify), on-demand smudge downloads.

### Library

Eight workspace crates published under the `git-lfs-*` prefix:
`git-lfs-pointer`, `git-lfs-store`, `git-lfs-git`, `git-lfs-api`,
`git-lfs-transfer`, `git-lfs-creds`, `git-lfs-filter`, and the
`git-lfs` binary.

[Unreleased]: https://gitlab.com/rustutils/git-lfs/-/compare/v0.7.0...HEAD
[0.7.0]: https://gitlab.com/rustutils/git-lfs/-/compare/v0.6.0...v0.7.0
[0.6.0]: https://gitlab.com/rustutils/git-lfs/-/compare/v0.5.0...v0.6.0
[0.5.0]: https://gitlab.com/rustutils/git-lfs/-/compare/v0.4.0...v0.5.0
[0.4.0]: https://gitlab.com/rustutils/git-lfs/-/compare/v0.3.0...v0.4.0
[0.3.0]: https://gitlab.com/rustutils/git-lfs/-/compare/v0.2.0...v0.3.0
[0.2.0]: https://gitlab.com/rustutils/git-lfs/-/tags/v0.2.0
