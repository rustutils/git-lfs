# git-lfs (Rust port)

A from-scratch Rust port of [Git LFS](https://github.com/git-lfs/git-lfs). The
goal is parity with the upstream Go binary at the CLI and protocol level, with
better help output and a clean split into library crates.

Most of upstream's CLI is implemented. `tests/SCOREBOARD.md` has the test pass
rates, and `NOTES.md` has the known gaps, open questions, and the path to 1.0.

## Layout

- `docs/`: protocol and format specs vendored from upstream. These are the
  contract we match. Link to them rather than paraphrasing them elsewhere.
- `tests/`: upstream's shell tests, vendored. They drive the binary through its
  CLI, so passing them means matching upstream's behavior.
- One top-level directory per crate, with short names (`cli/`, `pointer/`,
  ...). Package names use the `git-lfs-` prefix.
- `NOTES.md`: deferred work and open questions. Code comments that say "see
  NOTES.md" point here, so keep each entry under the crate or command it's
  about.

## Architecture

| Dir         | Package            | Purpose                                                  | Depends on                    |
| ----------- | ------------------ | -------------------------------------------------------- | ----------------------------- |
| `pointer/`  | `git-lfs-pointer`  | Pointer file parsing and encoding (`docs/spec.md`)       |                               |
| `store/`    | `git-lfs-store`    | Local object store under `.git/lfs/objects/`             | pointer                       |
| `git/`      | `git-lfs-git`      | Git config, attributes, refs, scanners, packet lines     | pointer                       |
| `creds/`    | `git-lfs-creds`    | Credential helpers                                       |                               |
| `api/`      | `git-lfs-api`      | Batch and locking HTTP client (`docs/api/`)              | creds                         |
| `transfer/` | `git-lfs-transfer` | Transfer queue, HTTPS and pure-SSH adapters              | api, store, pointer           |
| `filter/`   | `git-lfs-filter`   | Clean and smudge filters, filter-process                 | pointer, store, git           |
| `cli/`      | `git-lfs` (binary) | The CLI, wiring the crates together                      | all of the above              |

Two more workspace members aren't published: `xtask/` (doc generation and
`cargo xtask test`) and `tests/cmd/` (test helpers written in Rust).

If something doesn't clearly fit one of these crates, raise it before adding a
new one.

### Decisions

- **Shell out to `git`.** Don't use gix or git2 for Git operations (refs,
  rev-list, cat-file). Upstream shells out too, and the shell tests are written
  against that behavior. Using gix crates to parse file formats, like
  gix-attributes and gix-glob, is fine.
- **Rust 2024 edition.** The minimum Rust version is `rust-version` in the
  root `Cargo.toml`, and CI checks that it still builds. Raise it when you
  need a newer feature, and note it in `CHANGELOG.md`.
- **tokio and reqwest with rustls.** Code is synchronous where disk I/O
  dominates (`pointer`, `store`, `git`, `filter`) and async for HTTP (`api`,
  `transfer`). Async code calls into `store` through `spawn_blocking`.
- **Workspace dependencies.** Any dependency used by two or more crates has its
  version in the root `[workspace.dependencies]`. Internal crates always go
  through it too, as `git-lfs-foo = { workspace = true }`, never as a path
  dependency.

### Behavior worth knowing

- `clean`, `smudge`, `filter-process`, `fsck`, `track`, `untrack`, and
  `migrate import` install the Git hooks as a side effect, like upstream.

## Working in this repo

- **Upstream is the reference for behavior.** When the docs are ambiguous, read
  upstream's Go code (`commands/command_*.go` for the CLI) rather than
  guessing. For protocols, prefer the specs in `docs/`.
- **Match behavior, not structure.** Write idiomatic Rust, not translated Go.
- **Keep the CLI compatible.** The shell tests rely on upstream's flags,
  output, and exit codes. Help text can differ.
- **Running the shell tests:** `cargo xtask test` runs them all and prints a
  summary. `cd tests && make ./t-version.sh` runs one suite. Some upstream
  helpers can't be built here, and NOTES.md lists the tests that need them.

## Writing

- Write tersely, for the intended reader. Add information only when that reader
  needs it.
- Avoid em dashes, semicolons between clauses, `/` meaning "or", and `+`
  meaning "and".
- Use code formatting only for things the reader types or matches exactly.
  Write concepts in prose.
- Wrap prose at 80 columns.
- A crate's `README.md` is for someone on crates.io deciding whether the crate
  solves their problem. Its `lib.rs` docs are for someone using or reading the
  code: an entry point to the main types and how they fit together. Don't
  list the modules there: rustdoc already shows each one with its summary.
- `CHANGELOG.md` is for users of the CLI: what changed for them, and which
  settings control it. No crate internals or test counts.
- Commit messages are one plain line, with no prefix like `docs:` and no
  Markdown. Add prose paragraphs only when more context is needed.
