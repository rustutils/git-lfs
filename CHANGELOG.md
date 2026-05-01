# Changelog

All notable changes to this project will be documented in this file.

The format is based on [Keep a Changelog](https://keepachangelog.com/en/1.1.0/),
and this project adheres to [Semantic Versioning](https://semver.org/spec/v2.0.0.html).

## [Unreleased]

### Added

- Client-cert mTLS via `http.sslCert` and `http.sslKey` (per-URL or
  global). Honored alongside `http.sslcainfo`'s pinned-CA verifier
  for the same TLS handshake.
- `git lfs update` (minimal) — (re-)installs the four LFS git hooks
  (`pre-push`, `post-checkout`, `post-commit`, `post-merge`) for the
  current repository. Outside any git repo, prints
  `"Not in a Git repository."` and exits 128. Hook-conflict UI,
  `--manual` mode, leading-space template migration, and the
  `lfs.<url>.access` migration are still pending — tracked in
  `NOTES.md`.
- `pre-push` now supports local-path remotes (`git push ../sibling`,
  `git push .`) by copying each reachable LFS object directly into
  the target repo's `lfs/objects/` (hardlink with copy fallback).

### Fixed

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

[Unreleased]: https://gitlab.com/rustutils/git-lfs/-/compare/v0.3.0...HEAD
[0.3.0]: https://gitlab.com/rustutils/git-lfs/-/compare/v0.2.0...v0.3.0
[0.2.0]: https://gitlab.com/rustutils/git-lfs/-/tags/v0.2.0
