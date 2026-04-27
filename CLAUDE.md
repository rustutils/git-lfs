# Project: git-lfs (Rust reimplementation)

A from-scratch Rust port of [git-lfs](https://github.com/git-lfs/git-lfs).
Goal: feature parity with the upstream Go binary at the CLI + wire-protocol
level, with a better `--help`/UX and a cleaner library split.

## Status

Milestone 1 (pointer + store + filter clean/smudge + filter-process + `install` + `track`) shipped, sync.

Milestone 2 complete: `api/` (batch + locking), `transfer/` (queue + basic adapter, up + down + verify), smudge → on-demand-download, `git/`'s scanner (`rev_list` + `cat_file --batch[-check]` + `scan_pointers` + `scan_tree`), `diff_index` parser, `git lfs fetch`, `pull`, `push`, and `pre-push` (the hook command — `git push` now transparently uploads LFS objects, branch deletes are no-ops, new branches use `refs/remotes/<remote>/*` as exclude, `GIT_LFS_SKIP_PUSH` honored). `uninstall` and `untrack` round out the install/track pair (uninstall preserves user-modified hooks). `creds/` provides credential resolution (in-memory cache + `git credential fill/approve/reject` bridge); `api::Client` does the 401 → fill → retry once → approve/reject loop and caches working creds for subsequent requests. `git::endpoint` resolves the LFS URL via the full priority chain: `GIT_LFS_URL` env → `lfs.url` (git config + `.lfsconfig`) → `remote.<name>.lfsurl` → derived from `remote.<name>.url` (with SSH/git URL → HTTPS rewriting). Read-only inspection trio shipped: `ls-files` (default tree walk, `--all` history walk, `-l/-s/-n/-d/-j` flags), `env` (version, endpoints, paths, filter config), `status` (default / `--porcelain` / `--json`, classifies blobs as LFS / Git / File). Locking trio shipped: `lock` (with conflict reporting), `locks` (with paginated list + `--verify` ours/theirs partition), `unlock` (by path or `--id`, with `--force`), all driven by the existing `api/src/locks.rs` client. Quick wins shipped: `version` (banner), `pointer` (debug helper — `--check`/`--strict`, `--file` build, `--pointer`/`--stdin` parse + compare), `fsck` (`--objects` verifies store contents hash to their pointer OIDs and quarantines corrupt files to `<lfs>/bad/<oid>`; `--pointers` flags non-canonical pointers; `--dry-run` skips quarantine). Object-store hygiene shipped: `prune` (deletes local LFS objects not reachable from HEAD's tree or unpushed commits — uses `Store::each_object` to walk the sharded `objects/` tree). `checkout` shipped: replaces pointer text with real content for everything in HEAD's tree (or a path-filtered subset), fetching missing objects on demand. `post-checkout`/`post-commit`/`post-merge` shipped as exit-0 stubs (real lockable-flag management deferred until `track --lockable` lands) — without them, every `git checkout` after `git lfs install` would fail because the installed hook scripts called missing subcommands. `migrate` complete (all three subcommands): `info` walks history and reports file extensions by total size; `import` rewrites history so matching files become LFS pointers (via `git fast-export --full-tree | transform | git fast-import --force`), or with `--no-rewrite` appends a single conversion commit on top of HEAD; `export` is the inverse — pointer blobs become raw content from the local LFS store. Subprocess plumbing lives in `migrate/pipeline.rs` so import and export share it. `.gitattributes` parser + matcher shipped in `git/src/attr.rs` (backed by `gix-attributes` + `gix-glob`): `track` lists patterns recursively across all `.gitattributes` + `.git/info/attributes` with source annotations, and `fsck --pointers` flags `unexpectedGitObject` for LFS-tracked paths whose blobs aren't pointers. `track` long tail filled in: `--lockable` / `--not-lockable` / `-l` (write side — replaces existing line in place when lockable state flips), pattern blocklist (`.gitattributes`, `.gitignore`, `.gitmodules`, `.lfsconfig` — both literal and via globs like `.git*` / `*`), `--dry-run`, `--verbose` / `-v` ("Found N files previously added…" + always-on "Touching" lines via `git ls-files`, with re-staging via `git add` outside dry-run), `--no-excluded`, `--json` (struct-derived single-space-indent format for shell-test diffability), `[lockable]` listing annotation, repo-context check (exit 128 when not in a work tree or run inside `.git/`), CRLF preservation when `.gitattributes` already has CRLF lines or `core.autocrlf=true` (or `=input` on Windows), pattern escaping (spaces → `[[:space:]]`, leading `#` → `\#`), and `./`-prefix normalization. Comment parsing in `.gitattributes` lines now matches `gitattributes(5)` — only a leading `#` starts a comment, so escaped patterns like `\#` survive idempotency checks. Lockable read-only enforcement shipped end-to-end: new `cli/src/lockable.rs` module owns the `git ls-files` workdir walk, the `verify_locks` "ours" query (lazy — only fired when at least one indexed path is lockable, so a `.gitattributes`-only commit doesn't churn credential-helper state), and the per-platform chmod (owner-write bit on Unix, `set_readonly` on Windows). Wired into `post-checkout` / `post-commit` / `post-merge` (full workdir scan), into `track --lockable` / `--not-lockable` (per-pattern, lazy held-locks query), into `git lfs lock` (chmod +w on success so the user can edit) and `git lfs unlock` (chmod -w if path is lockable). `track` also auto-installs the four LFS hooks (mirroring upstream's `installHooks(false)`), gated on `GIT_LFS_TRACK_NO_INSTALL_HOOKS` and best-effort (silently skips user-edited hook files). `create_lock` and `delete_lock` now route through the auth-retry loop (a new `Client::send_with_auth_retry_response` helper wraps the 401 → fill → retry → approve/reject dance for endpoints with bespoke status handling — `create_lock` keeps its 409 → `Conflict { existing, message }` decoding on top of it). `LockList` and `VerifyLocksResponse` deserialize `null` arrays as empty (Go's `encoding/json` serializes `nil` slices as `null`, which the upstream `lfstest-gitserver` inherits). `filter-process` handshake bug fixed (advertise server caps preemptively before reading client caps) — was deadlocking `git add` of LFS-tracked paths and the upstream shell tests that exercise that path. Milestone 3 territory now: `merge-driver`, `dedup`, `ext`, `standalone-file`, `logs`, `update`, plus deferred polish across the shipped commands (NOTES.md). The full read+write loop works end-to-end against authenticated LFS endpoints with no explicit `lfs.url` config. Milestone 3 territory: custom transfer adapter protocol, SSH `git-lfs-authenticate`, netrc / askpass / NTLM / Kerberos, `migrate` history rewriting, and the rest of the long tail.

## Layout

- `docs/` — vendored upstream protocol/format specs (authoritative). Treat
  these as the contract we have to match. Do not paraphrase them into other
  files — link to them.
- `tests/` — vendored upstream shell integration tests. They drive the binary
  via its CLI, so passing them ⇒ behavioral parity. Strongest safety net.
- Workspace crates live as flat top-level directories with short names
  (`cli/`, `pointer/`, `store/`, …). Package names inside their `Cargo.toml`
  use the full `git-lfs-*` prefix. See Architecture below.
- `LICENSE.md` — MIT, with attribution to upstream Git LFS contributors.
- `NOTES.md` — deferred items, open questions, milestone tracking.
  Source-code comments (`see NOTES.md`) point here; keep entries scoped
  to one crate or command so they're easy to find by section.
- `CLAUDE.md` (this file) — present-tense project conventions, layout, and
  working rules. Auto-loaded when working with Claude.

## Architecture

Cargo workspace, seven library crates + one binary. Crate names use the
crates.io-style `git-lfs-*` prefix (publish-ready, no future collision).

| Dir         | Package name        | Purpose                                                              | Depends on                    |
| ----------- | ------------------- | -------------------------------------------------------------------- | ----------------------------- |
| `pointer/`  | `git-lfs-pointer`   | parse/encode pointer files (`docs/spec.md`)                          | —                             |
| `store/`    | `git-lfs-store`     | content-addressable object store at `.git/lfs/objects/{OID-PATH}`    | pointer                       |
| `git/`      | `git-lfs-git`       | git interop: config, attrs, refs, scanners, filter-process protocol  | —                             |
| `api/`      | `git-lfs-api`       | batch + locking HTTP client (`docs/api/`)                            | —                             |
| `transfer/` | `git-lfs-transfer`  | transfer queue + adapters (basic, tus, custom, ssh)                  | api, store, pointer           |
| `creds/`    | `git-lfs-creds`     | credential helper bridge                                             | git                           |
| `filter/`   | `git-lfs-filter`    | clean/smudge filters                                                 | pointer, store, transfer, git |
| `cli/`      | `git-lfs` (bin)     | CLI surface, wires everything together                               | all of the above              |

If something doesn't obviously fit one of these crates, raise it before
inventing a new one.

### Key tech decisions

- **Git interop: shell out to `git`.** Not `gix`, not `git2`. Upstream Go
  shells out for almost everything, and the vendored `tests/` are written
  against that behavior, so shelling out gives 1:1 parity on edge cases
  (attributes, refs, filter-process framing) without chasing pure-Rust git
  library coverage gaps. Hot-path optimization to `gix` is a possible v2
  move, not a v0 concern.
  - The "no gix" rule is about *runtime operations* — git-lfs uses whichever
    `git` binary the user has installed, never bundling its own. Pulling
    `gix-*` crates as parsing libraries (`gix-attributes`, `gix-glob`, …)
    is fine: those parse stable file formats and don't replace the system
    git. The rule kicks in when proposing to use `gix` to *do* git
    operations (refs, rev-list, cat-file) instead of shelling out.
- **Edition: 2024.** No MSRV pinned for now.
- **Async stack: tokio + reqwest (rustls).** Sync everywhere disk I/O dominates
  (`pointer/`, `store/`, `git/`, `filter/`); async kicks in at `api/` (HTTP)
  and will dominate `transfer/` (concurrent transfers). When async code needs
  to call into the sync `store/`, route through `tokio::task::spawn_blocking`.
  reqwest uses `rustls-tls` (no system openssl) + `json` + `http2` + `charset`.

### Dependency conventions

- **Workspace dependencies for anything shared.** If two or more crates pull
  in the same external dep, hoist its version into the root
  `[workspace.dependencies]` table and have each crate declare it as
  `dep = { workspace = true }` (plus per-crate `features = [...]` as needed).
  Single source of truth for the version.
- **Internal crates always go through `[workspace.dependencies]`.** Even when
  only one consumer exists today. The path lives in one place at the root, so
  moving/renaming a crate is a one-line change. Member crates depend via
  `git-lfs-foo = { workspace = true }` — never inline `{ path = "../foo" }`.

## Working with this repo

- **Source of truth for behavior:** when docs are ambiguous, grep the upstream
  Go code at <https://github.com/git-lfs/git-lfs> (`commands/command_*.go`
  for CLI surface). Don't guess — they've already solved it.
- **Running upstream integration tests:** `cd tests && make test` builds the
  Go test helpers (vendored under `tests/cmd/`), the Rust release binary
  (copied to `bin/git-lfs`), and runs the upstream `t-*.sh` shell tests via
  `prove`. A single test: `cd tests && make ./t-version.sh`. Three upstream
  helpers are excluded (need internal Go packages we'd have to vendor); the
  tests that exercise them are listed as skipped in NOTES.md. Long-term we
  want to port the suite to native `cargo test`; tracked in NOTES.md.
- **Don't translate Go to Rust line-for-line.** The point of the rewrite is to
  use idiomatic Rust + better libraries. Match behavior, not structure.
- **CLI compatibility is a hard constraint.** The shell tests in `t/` assume
  the upstream CLI surface. If you want to diverge (e.g. better `--help`),
  preserve the underlying flags + exit codes the tests rely on.
- **Prefer reading `docs/api/` over reimplementing protocol logic from
  upstream code.** The docs are the spec; the Go is one implementation of it.

## See also

- `NOTES.md` — milestones, vendored-vs-skipped rationale, open questions.
