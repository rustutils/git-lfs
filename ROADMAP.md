# Roadmap

Where this project is heading and how we plan to get there. Versions
between today and 1.0 are intentionally rough — we'll cut releases as
the work lands rather than holding up shipping for a perfect milestone
boundary.

## Today

**v0.3.0** — released 2026-05-01. 523 of 794 vendored upstream shell
tests pass (~66%) across 31 full-pass suites. Day-to-day flows
(clean / smudge, fetch / pull / push, track, locking, migrate) work
end-to-end. See [`tests/SCOREBOARD.md`](tests/SCOREBOARD.md) for the
per-suite breakdown.

## Path to 1.0

The bar for 1.0 is full feature parity with upstream Go git-lfs:

- 100% pass on the vendored upstream shell test suite.
- Comprehensive, polished documentation.
- Polished `--help` output and man pages across every subcommand.
- Packaged binary releases for Linux and macOS (Windows if there's
  demand).
- A code-style and design audit, performed independently on each side.
- A comparative audit against upstream Go git-lfs to surface any
  divergence the test suite doesn't catch.

## Step releases

These are step targets, not contracts — the actual cuts will be
opportunistic.

### v0.4

Goals:

- 75% test coverage (~+73 tests): Knock out the almost-done sweeps 
  (7 partially-passing suites that are each one or two tests away 
  from a full pass) and the cheap half of the long-tail polish 
  (`t-prune`, `t-ls-files`, `t-pointer`, `t-install`, `t-clone`).
- Initial sweep over man pages and help text. Make sure every help
  text and man page works, explains high-level concept at the right
  level of detail.

### v0.5 

Goals:

- 85% test coverage (~+80 tests beyond v0.4): Networking + credentials 
  cluster: `t-credentials`, `t-credentials-no-prompt`, `t-credentials-protect`, 
  `t-askpass`, `t-extra-header`, `t-content-type`, `t-fetch-recent`, 
  `t-expired`. Plus retry / `Retry-After` handling, which clears the
  `t-batch-retries-*` cluster.
-  Polished docs, man pages, help text. What this means:
  - Every man page should have the right sections (bug reporting, 
    examples)
  - Help text should have arguments grouped into headings, where
    appropriate
  - Hosted docs should be comprehensive (installation instructions)
  - Style sweep over the prose


### v0.6 

Goals:

- 90% test coverage
- CI-driven binary builds for Linux (musl only?) and macOS 
  (x86_64 + aarch64), tagged release artifacts on GitLab + mirrored
  on GitHub for discoverability.
- A subset of the remaining greenfield commands and protocols: custom transfer
  adapters + tus, SSH (`git-lfs-authenticate`), merge-driver,
  smudge-side pointer extensions, `dedup`, `completion`, `logs`,
  `update`, `standalone-file`, `t-multiple-remotes`.


### v0.7

Goals:

- 95% test coverage
- A the full remaining greenfield commands and protocols: custom transfer
  adapters + tus, SSH (`git-lfs-authenticate`), merge-driver,
  smudge-side pointer extensions, `dedup`, `completion`, `logs`,
  `update`, `standalone-file`, `t-multiple-remotes`.
- An internal audit of the codebase: reviewing every flow, making sure
  that (a) the coding style legible and refactored, (b) invariants and
  algorithms are documented, and (c) there are no obvious bugs we can spot

### v0.8

Goals:

- 100% test coverage
- A comparative audit with the Go git-lfs codebase, making sure that
  we did not miss any functionality that is not expressed through the
  test suite, or that our behaviour does not differ significantly
  (correctness).

### v1.0

Goals:

- Initial stable release
- Full parity with Go git-lfs

## Open questions

- **Windows support and packaging.** Worth doing speculatively, or 
  wait for someone to ask? Likely not worth the effort at this
  point.
- **Hot-path optimization.** The current "shell out to `git`" model is
  great for parity, but a `gix` integration could be a meaningful
  speedup on large fetches/scans. Out of scope for 1.0; flag for a
  later major.
- **Crate API stability.** The eight `git-lfs-*` library crates are
  published but their public API isn't pinned yet. Decide before 1.0
  whether the binary and the libraries share a version.
