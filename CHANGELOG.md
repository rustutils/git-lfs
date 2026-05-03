# Changelog

All notable changes to this project will be documented in this file.

The format is based on [Keep a Changelog](https://keepachangelog.com/en/1.1.0/),
and this project adheres to [Semantic Versioning](https://semver.org/spec/v2.0.0.html).

## [Unreleased]

### Fixed

- `git lfs fetch <ref>...` now scans only the HEAD-state of each
  named ref instead of walking its full history. Historical /
  deleted-from-HEAD pointers still get fetched via `--all` or
  `--recent`. Matches upstream's `fetchRef` vs `fetchRefs` split
  and is a prerequisite for the upcoming `--recent` semantics.

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

[Unreleased]: https://gitlab.com/rustutils/git-lfs/-/compare/v0.5.0...HEAD
[0.5.0]: https://gitlab.com/rustutils/git-lfs/-/compare/v0.4.0...v0.5.0
[0.4.0]: https://gitlab.com/rustutils/git-lfs/-/compare/v0.3.0...v0.4.0
[0.3.0]: https://gitlab.com/rustutils/git-lfs/-/compare/v0.2.0...v0.3.0
[0.2.0]: https://gitlab.com/rustutils/git-lfs/-/tags/v0.2.0
