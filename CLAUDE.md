# Project: git-lfs (Rust reimplementation)

A from-scratch Rust port of [git-lfs](https://github.com/git-lfs/git-lfs).
Goal: feature parity with the upstream Go binary at the CLI + wire-protocol
level, with a better `--help`/UX and a cleaner library split.

## Status

Milestone 1 (pointer + store + filter clean/smudge + filter-process + `install` + `track`) shipped, sync.

Milestone 2 complete: `api/` (batch + locking), `transfer/` (queue + basic adapter, up + down + verify), smudge → on-demand-download, `git/`'s scanner (`rev_list` + `cat_file --batch[-check]` + `scan_pointers`), `git lfs fetch`, `pull`, `push`, and `pre-push` (the hook command — `git push` now transparently uploads LFS objects, branch deletes are no-ops, new branches use `refs/remotes/<remote>/*` as exclude, `GIT_LFS_SKIP_PUSH` honored). `uninstall` and `untrack` round out the install/track pair (uninstall preserves user-modified hooks). `creds/` provides credential resolution (in-memory cache + `git credential fill/approve/reject` bridge); `api::Client` does the 401 → fill → retry once → approve/reject loop and caches working creds for subsequent requests. `git::endpoint` resolves the LFS URL via the full priority chain: `GIT_LFS_URL` env → `lfs.url` (git config + `.lfsconfig`) → `remote.<name>.lfsurl` → derived from `remote.<name>.url` (with SSH/git URL → HTTPS rewriting). The full read+write loop works end-to-end against authenticated LFS endpoints with no explicit `lfs.url` config. Milestone 3 territory: locking command surface (`lock`/`unlock`/`locks`), custom transfer adapter protocol, SSH `git-lfs-authenticate`, netrc / askpass / NTLM / Kerberos.

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
- `NOTES.md`, `PLAN.md`, `CLAUDE.md` — local-only working notes (gitignored
  via `.git/info/exclude`). Use these for project state instead of memory.

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
  Go code at `/home/patrick/Projects/temp/git-lfs`. Don't guess — they've
  already solved it.
- **Don't translate Go to Rust line-for-line.** The point of the rewrite is to
  use idiomatic Rust + better libraries. Match behavior, not structure.
- **CLI compatibility is a hard constraint.** The shell tests in `t/` assume
  the upstream CLI surface. If you want to diverge (e.g. better `--help`),
  preserve the underlying flags + exit codes the tests rely on.
- **Prefer reading `docs/api/` over reimplementing protocol logic from
  upstream code.** The docs are the spec; the Go is one implementation of it.

## See also

- `NOTES.md` — milestones, vendored-vs-skipped rationale, open questions.
