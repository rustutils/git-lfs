# Project: git-lfs (Rust reimplementation)

A from-scratch Rust port of [git-lfs](https://github.com/git-lfs/git-lfs).
Goal: feature parity with the upstream Go binary at the CLI + wire-protocol
level, with a better `--help`/UX and a cleaner library split.

## Status

**Milestone 1 (sync foundation)** — shipped. Pointer parse/encode,
content-addressable store, clean/smudge filters, filter-process protocol,
`install`, `track`.

**Milestone 2 (read+write loop)** — shipped. The full clone → fetch → smudge
→ edit → clean → push cycle works end-to-end against authenticated LFS
endpoints with no explicit `lfs.url` config.

- **Networking.** `api/` does the batch + locking HTTP client; `creds/`
  bridges `git credential fill/approve/reject` and runs the 401 → fill →
  retry-once → approve/reject loop with an in-memory cache.
  `git::endpoint` walks the full upstream URL priority chain: `GIT_LFS_URL`
  → `lfs.url` (git config + `.lfsconfig`) → `remote.<name>.lfsurl` →
  derived from `remote.<name>.url` (SSH/git URL → HTTPS rewriting). The
  batch response decoder tolerates servers that omit `size`.
- **Transfer.** `transfer/` is a concurrent queue with the basic adapter
  (up + down + verify). Smudge does on-demand download.
- **Git interop.** `git/` ships the scanner (`rev_list`, `cat-file
  --batch[-check]`, `scan_pointers`, `scan_tree`), `diff_index`, refspec
  resolution (`branch.<x>.merge` / current ref), and a `.gitattributes`
  parser+matcher backed by `gix-attributes` + `gix-glob`.
- **Commands.** `fetch`, `pull`, `push`, `pre-push`, `clone` (deprecated
  upstream wrapper), `checkout`, `fsck`, `prune`, `migrate` (info /
  import / export — full history rewriting via `fast-export | transform |
  fast-import`), `lock` / `locks` / `unlock`, `ls-files`, `env`, `status`,
  `track` / `untrack`, `install` / `uninstall`, `version`, `pointer`. All
  four hook entry points (`pre-push`, `post-checkout`, `post-commit`,
  `post-merge`) ship.
- **Lockable invariant.** `cli/src/lockable.rs` owns the workdir walk, the
  lazy `verify_locks` "ours" query, and the per-platform chmod (owner-
  write bit on Unix, `set_readonly` on Windows). Honors both
  `lfs.setlockablereadonly` config and `GIT_LFS_SET_LOCKABLE_READONLY`.
- **Hook auto-install.** `clean`, `smudge`, `filter-process`, `fsck`,
  `track`, `untrack`, `migrate import` all best-effort install the four
  hooks on invocation, mirroring upstream's `installHooks(false)`
  side-effect pattern.
- **Pre-push lock verification.** `cli/src/locks_verify.rs` reads
  `lfs.<endpoint>.locksverify` (falling back to `lfs.locksverify`) and
  runs the full setting × status matrix (enabled / unset / disabled ×
  200 / 5xx / 501 / 403 / 404).
- **Test scaffolding.** `lfstest-testutils addcommits` ported to Rust at
  `tests/cmd/src/bin/lfstest-testutils.rs` so the ~11 fixture-building
  shell tests can run without the upstream Go testtools. Lives in its
  own `lfstest` crate (publish=false), so `cargo install git-lfs`
  installs only the production binary.

**Milestone 3 territory** — not started or partial. Custom transfer
adapters + tus, pure-SSH transfer (`git-lfs-transfer`), netrc / NTLM /
Kerberos / mTLS, `merge-driver`, `dedup`, `ext`, `standalone-file`,
`logs`, `completion`, retry / Retry-After / rate-limit handling,
fetch-recent semantics, full prune retention. SSH `git-lfs-authenticate`
itself shipped (M8a). See `NOTES.md` for the ranked gap list and
per-command deferred polish.

**Released as v0.2.0 on crates.io.** All eight workspace members are
publish-ready (description, keywords, categories, repository, per-crate
README). Project status remains experimental — for production use,
upstream Go `git-lfs` is still the answer; ~85% of the vendored shell
tests pass (673/794 across 104 files).

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
