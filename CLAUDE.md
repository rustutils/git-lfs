# Project: git-lfs (Rust reimplementation)

A from-scratch Rust port of [git-lfs](https://github.com/git-lfs/git-lfs).
Goal: feature parity with the upstream Go binary at the CLI + wire-protocol
level, with a better `--help`/UX and a cleaner library split.

## Status

Experimental, but already covers most of the upstream surface. The
full clone → fetch → smudge → edit → clean → push cycle works
end-to-end against authenticated LFS endpoints. All eight workspace
members are publish-ready and shipped on crates.io.

What's implemented today:

- The CLI surface — `fetch`, `pull`, `push`, `pre-push`, `clone`
  (deprecated upstream wrapper), `checkout`, `fsck`, `prune`,
  `migrate` (info / import / export), `lock` / `locks` / `unlock`,
  `ls-files`, `env`, `status`, `track` / `untrack`, `install` /
  `uninstall`, `merge-driver`, `logs`, `version`, `pointer`, plus
  the four hook entry points.
- Networking — `api/` for batch + locking; `creds/` for the
  `git credential fill / approve / reject` bridge plus netrc and
  askpass; `git::endpoint` for the full URL priority chain;
  `transfer/` for the concurrent queue with the basic adapter
  (upload, download, verify, Retry-After, range-resume).
- Git interop — `git/` ships the scanner (`rev_list`, `cat-file
  --batch[-check]`, `scan_pointers`, `scan_tree`), `diff_index`,
  refspec resolution, and a `.gitattributes` parser + matcher
  backed by `gix-attributes` + `gix-glob`.
- Lockable invariant — `cli/src/lockable.rs` owns the workdir
  walk, the lazy `verify_locks` "ours" query, and the per-platform
  chmod (Unix owner-write bit, Windows `set_readonly`).
- Hook auto-install — `clean`, `smudge`, `filter-process`, `fsck`,
  `track`, `untrack`, `migrate import` all best-effort install
  the four hooks on invocation, mirroring upstream's
  `installHooks(false)` side-effect pattern.
- Pre-push lock verification — `cli/src/locks_verify.rs` honors
  `lfs.<endpoint>.locksverify` (falling back to `lfs.locksverify`)
  across the full setting × status matrix.
- Test scaffolding — `lfstest-testutils addcommits` ported to Rust
  at `tests/cmd/src/bin/lfstest-testutils.rs` so the
  fixture-building shell tests run without the upstream Go
  testtools. Lives in its own `lfstest` crate (publish=false), so
  `cargo install git-lfs` installs only the production binary.

What's still gapped: custom transfer adapters + tus, pure-SSH
transfer (`git-lfs-transfer`), NTLM / Kerberos / mTLS, multi-stage
auth (`wwwauth[]` / `state[]`), `dedup`, `completion`, full
fetch-recent / prune retention semantics. See `NOTES.md` for the
ranked gap list, per-command deferred polish, and
`tests/SCOREBOARD.md` for the live per-suite pass rate.

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

- `NOTES.md`: milestones, vendored-vs-skipped rationale, open questions.
- `ROADMAP.md`: high-level roadmap for the projects (milestones)
