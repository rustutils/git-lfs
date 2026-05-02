//! Clap CLI surface (struct `Cli` + subcommands).
//!
//! Extracted from `main.rs` so xtask (and any future tool) can
//! reuse the command tree for man-page generation, completion
//! scripts, etc. Keep this file focused on the clap derive — all
//! dispatch / business logic stays in main.rs and the per-command
//! modules.
//!
//! Each subcommand is a tuple variant on [`Command`] delegating to
//! a `*Args` struct. The struct is the home for the rustdoc that
//! drives clap's `about` / `long_about` (first paragraph → about,
//! rest → long_about) and for `#[command(...)]` extras such as
//! `after_help`, aliases, and arg-group headings. Keep the variants
//! themselves bare — putting a doc comment on the variant would
//! shadow the struct's docs.

use std::path::PathBuf;

use clap::{Args, Parser, Subcommand};

#[derive(Parser)]
#[command(
    name = "git-lfs",
    about = "Git LFS — large file storage for git",
    // We want `git lfs --version` to print the same banner as
    // `git lfs version`. clap's auto-derived `--version` would
    // emit `git-lfs <version>` (one token, no `/` separator),
    // which doesn't match the user-agent style upstream uses.
    // Suppress clap's flag and handle --version ourselves.
    disable_version_flag = true,
    max_term_width = 100,
)]
pub struct Cli {
    /// Print the version banner and exit.
    #[arg(long, short = 'V', global = true)]
    pub version: bool,

    #[command(subcommand)]
    pub command: Option<Command>,
}

// note: don't add rustdoc comments here, they will shadow the struct's docs
// in the clap-generated help output
#[derive(Subcommand)]
pub enum Command {
    Clean(CleanArgs),
    Smudge(SmudgeArgs),
    Install(InstallArgs),
    Uninstall(UninstallArgs),
    Track(TrackArgs),
    Untrack(UntrackArgs),
    FilterProcess(FilterProcessArgs),
    Fetch(FetchArgs),
    Pull(PullArgs),
    Push(PushArgs),
    Clone(CloneArgs),
    PostCheckout(PostCheckoutArgs),
    PostCommit(PostCommitArgs),
    PostMerge(PostMergeArgs),
    PrePush(PrePushArgs),
    Version(VersionArgs),
    Pointer(PointerArgs),
    Env(EnvArgs),
    Ext(ExtArgs),
    Update(UpdateArgs),
    Migrate(MigrateArgs),
    Checkout(CheckoutArgs),
    Prune(PruneArgs),
    Fsck(FsckArgs),
    Status(StatusArgs),
    Lock(LockArgs),
    Locks(LocksArgs),
    Unlock(UnlockArgs),
    LsFiles(LsFilesArgs),
}

/// Git clean filter that converts large files to pointers
///
/// Read the contents of a large file from standard input, and write a
/// Git LFS pointer file for that file to standard output.
///
/// Clean is typically run by Git’s clean filter, configured by the
/// repository’s Git attributes.
///
/// Clean is not part of the user-facing Git plumbing commands.
/// To preview the pointer of a large file as it would be generated,
/// see the git-lfs-pointer(1) command.
#[derive(Args)]
pub struct CleanArgs {
    /// Working-tree path of the file being cleaned.
    ///
    /// Substituted for `%f` in any configured `lfs.extension.<name>.clean` command.
    pub path: Option<PathBuf>,
}

/// Git smudge filter that converts pointer in blobs to the actual content
///
/// Read a Git LFS pointer file from standard input and write the contents of the
/// corresponding large file to standard output. If needed, download the file’s
/// contents from the Git LFS endpoint. The argument, if provided, is only used
/// for a progress bar.
///
/// Smudge is typically run by Git’s smudge filter, configured by the repository’s
/// Git attributes.
///
/// In your Git configuration or in a .lfsconfig file, you may set either or both
/// of `lfs.fetchinclude` and `lfs.fetchexclude` to comma-separated lists of paths.
/// If `lfs.fetchinclude` is defined, Git LFS pointer files will only be replaced
/// with the contents of the corresponding Git LFS object file if their path
/// matches one in that list, and if `lfs.fetchexclude` is defined, Git LFS pointer
/// files will only be replaced with the contents of the corresponding Git LFS
/// object file if their path does not match one in that list. Paths are matched
/// using wildcard matching as per gitignore(5). Git LFS pointer files that are
/// not replaced with the contents of their corresponding object files are simply
/// copied to standard output without change.
///
/// Without any options, git lfs smudge outputs the raw Git LFS content to standard
/// output.
#[derive(Args)]
pub struct SmudgeArgs {
    /// Working-tree path of the file being smudged (currently unused).
    pub path: Option<PathBuf>,
    /// Skip automatic downloading of objects on clone or pull.
    ///
    /// Equivalent to `GIT_LFS_SKIP_SMUDGE=1`. Wired up by `git lfs install --skip-smudge`.
    #[arg(long)]
    pub skip: bool,
}

/// Configure git to invoke git-lfs as the clean/smudge/process filter,
/// and install the LFS git hooks.
#[derive(Args)]
pub struct InstallArgs {
    // TODO(post-1.0): replace the --local/--system/--worktree/--file mutex
    // with a clap ArgGroup (multiple = false). Validation lives in
    // resolve_install_scope (cli/src/main.rs); kept manual because
    // tests/t-install.sh:329 (and the t-install-worktree / t-uninstall /
    // t-uninstall-worktree variants) assert upstream's exact wording
    // ("Only one of the --local, --system, --worktree, and --file
    // options can be specified."). Worth taking once we're free to
    // update those assertions.
    /// Set config in the local repo only (default: --global).
    #[arg(short, long)]
    pub local: bool,
    /// Operate on `/etc/gitconfig` (`git config --system`).
    #[arg(long)]
    pub system: bool,
    /// Operate on `.git/config.worktree` for the current worktree.
    #[arg(long)]
    pub worktree: bool,
    /// Operate on the given config file directly. Treated as
    /// "global-like" for the success message.
    #[arg(long, value_name = "PATH")]
    pub file: Option<PathBuf>,
    /// Overwrite existing config and hooks.
    #[arg(short, long)]
    pub force: bool,
    /// Only set the filter config; don't install hooks.
    #[arg(long)]
    pub skip_repo: bool,
    /// Configure the smudge filter to pass pointer text through
    /// unchanged. Use with a follow-up `git lfs pull` to download
    /// content on demand.
    #[arg(long)]
    pub skip_smudge: bool,
}

/// Reverse of `install`: clear the `filter.lfs.*` config and remove
/// the LFS git hooks. Hooks that don't match what we'd write are left
/// untouched.
#[derive(Args)]
pub struct UninstallArgs {
    // TODO(post-1.0): same --local/--system/--worktree/--file mutex as
    // InstallArgs — share a clap ArgGroup. See InstallArgs's TODO for
    // the rationale and test references.
    /// Optional mode: `hooks` removes only the LFS git hooks and
    /// leaves the filter config alone (the inverse of `--skip-repo`).
    pub mode: Option<String>,
    /// Operate on the local repo only (default: --global).
    #[arg(short, long)]
    pub local: bool,
    /// Operate on `/etc/gitconfig` (`git config --system`).
    #[arg(long)]
    pub system: bool,
    /// Operate on `.git/config.worktree` for the current worktree.
    #[arg(long)]
    pub worktree: bool,
    /// Operate on the given config file directly. Treated as
    /// "global-like" for the success message.
    #[arg(long, value_name = "PATH")]
    pub file: Option<PathBuf>,
    /// Only unset config; don't touch hooks.
    #[arg(long)]
    pub skip_repo: bool,
}

/// Track a file pattern with git-lfs by adding it to .gitattributes.
/// With no patterns, lists currently-tracked patterns.
#[derive(Args)]
pub struct TrackArgs {
    /// File patterns to track (e.g. "*.jpg", "data/*.bin").
    pub patterns: Vec<String>,
    /// Mark the tracked pattern as `lockable` (`*.psd lockable`).
    #[arg(short = 'l', long)]
    pub lockable: bool,
    /// Re-track an existing pattern, removing its `lockable` flag.
    #[arg(long)]
    pub not_lockable: bool,
    /// Print what would happen without modifying `.gitattributes` or
    /// re-staging files.
    #[arg(long)]
    pub dry_run: bool,
    /// Extra logging: print "Found N files previously added to Git
    /// matching pattern" lines.
    #[arg(short, long)]
    pub verbose: bool,
    /// Listing mode only: emit JSON instead of the human-readable
    /// listing.
    #[arg(long)]
    pub json: bool,
    /// Listing mode only: suppress the "Listing excluded patterns"
    /// section.
    #[arg(long)]
    pub no_excluded: bool,
    /// Treat each pattern as a literal filename — escape glob
    /// metacharacters (`*`, `?`, `[`, `]`, backslash, space) so
    /// the entry in `.gitattributes` matches that exact name even
    /// when it contains shell-glob characters.
    #[arg(long)]
    pub filename: bool,
    /// Don't modify `.gitattributes` — the user has already added
    /// the LFS filter line themselves. Still walks the index and
    /// touches matching files' mtime so they show as modified on
    /// the next `git status`.
    #[arg(long)]
    pub no_modify_attrs: bool,
}

/// Stop tracking a file pattern with git-lfs by removing it from
/// .gitattributes. The matching pointer files in history (and the
/// objects in the local store) are left in place.
#[derive(Args)]
pub struct UntrackArgs {
    /// File patterns to untrack.
    pub patterns: Vec<String>,
}

/// Run the long-running filter-process protocol with git over stdin/stdout.
/// This is what git invokes via filter.lfs.process and is the batched
/// alternative to per-invocation `clean`/`smudge`.
#[derive(Args)]
pub struct FilterProcessArgs {
    /// Pass smudge requests' pointer text through unchanged;
    /// equivalent to `GIT_LFS_SKIP_SMUDGE=1`. Wired up by
    /// `install --skip-smudge`.
    #[arg(long)]
    pub skip: bool,
}

/// Download every LFS object reachable from the given refs (default: HEAD)
/// that isn't already in the local store. Walks history, dedupes by OID.
#[derive(Args)]
pub struct FetchArgs {
    /// First positional arg is treated as a remote name (if it
    /// resolves); subsequent args are refs.
    pub args: Vec<String>,
    /// List the objects that would be fetched without downloading
    /// them (one `fetch <oid> => <path>` line per object).
    #[arg(long)]
    pub dry_run: bool,
    /// JSON output. With `--dry-run`, queries the server's batch
    /// endpoint to populate `actions` URLs.
    #[arg(long)]
    pub json: bool,
    /// Walk every local ref under `refs/heads/*` + `refs/tags/*`.
    #[arg(long)]
    pub all: bool,
    /// Re-download objects we already have (e.g. recovery from a
    /// corrupt local store).
    #[arg(long)]
    pub refetch: bool,
    /// Read refs from stdin, one per line. Blank lines dropped.
    #[arg(long)]
    pub stdin: bool,
    /// Run `prune` after the fetch completes.
    #[arg(long)]
    pub prune: bool,
    /// Comma-separated globs; only matching paths are fetched.
    /// Falls back to `lfs.fetchinclude` when omitted.
    #[arg(short = 'I', long)]
    pub include: Vec<String>,
    /// Comma-separated globs; matching paths are skipped. Falls
    /// back to `lfs.fetchexclude` when omitted.
    #[arg(short = 'X', long)]
    pub exclude: Vec<String>,
}

/// `fetch` then re-run the smudge filter so the working tree contains
/// real LFS file contents instead of pointer text. Requires
/// `git lfs install` to have wired up the smudge filter.
#[derive(Args)]
pub struct PullArgs {
    /// Refs to scan for LFS pointers. Defaults to `HEAD`.
    pub refs: Vec<String>,
    /// Comma-separated globs; only matching paths are pulled.
    /// Falls back to `lfs.fetchinclude` when omitted.
    #[arg(short = 'I', long)]
    pub include: Vec<String>,
    /// Comma-separated globs; matching paths are skipped. Falls
    /// back to `lfs.fetchexclude` when omitted.
    #[arg(short = 'X', long)]
    pub exclude: Vec<String>,
}

/// Upload every LFS object reachable from the given refs that the
/// remote doesn't already have. The "doesn't have" set is approximated
/// by `refs/remotes/<remote>/*`; the LFS server's batch API also
/// dedupes server-side so missing exclusions don't waste bandwidth.
#[derive(Args)]
pub struct PushArgs {
    /// Name of the remote (e.g. "origin") whose tracking refs are
    /// excluded from the upload set.
    pub remote: String,
    /// Refs (or, with `--object-id`, raw OIDs) to push. With
    /// `--all`, restricts the all-refs walk to these; with
    /// `--stdin`, ignored (a warning is emitted).
    pub args: Vec<String>,
    /// List the objects that would be pushed without actually
    /// uploading them (one `push <oid> => <path>` line per object).
    #[arg(long)]
    pub dry_run: bool,
    /// Push every local ref under `refs/heads/*` and `refs/tags/*`
    /// (intersected with `args` if any are given).
    #[arg(long)]
    pub all: bool,
    /// Read refs (or OIDs, with `--object-id`) from stdin, one per
    /// line. Blank lines are skipped.
    #[arg(long)]
    pub stdin: bool,
    /// Treat positional args / stdin entries as raw LFS OIDs
    /// rather than git refs, and upload those objects directly
    /// from the local store.
    #[arg(long)]
    pub object_id: bool,
}

/// Deprecated. Wraps `git clone` so the working tree is populated
/// with pointer text first, then runs `git lfs pull` to download
/// LFS content in batch. Modern `git clone` parallelizes the
/// smudge filter and is no slower; prefer it.
#[derive(Args)]
pub struct CloneArgs {
    /// `git clone` and LFS pass-through args. The repository URL
    /// is required; an optional target directory follows.
    #[arg(trailing_var_arg = true, allow_hyphen_values = true)]
    pub args: Vec<String>,
}

/// Git post-checkout hook entry point. Receives `<prev-sha>
/// <post-sha> <flag>` (flag is "1" if HEAD moved). Currently a
/// no-op stub — exists so installed hook scripts don't fail. Real
/// behavior arrives with `track --lockable`.
#[derive(Args)]
pub struct PostCheckoutArgs {
    pub args: Vec<String>,
}

/// Git post-commit hook entry point. No arguments. Currently a
/// no-op stub.
#[derive(Args)]
pub struct PostCommitArgs {
    pub args: Vec<String>,
}

/// Git post-merge hook entry point. Receives `<squash-flag>`.
/// Currently a no-op stub.
#[derive(Args)]
pub struct PostMergeArgs {
    pub args: Vec<String>,
}

/// Git pre-push hook entry point — not typically invoked by hand.
/// Reads `<local-ref> <local-sha> <remote-ref> <remote-sha>` lines
/// from stdin and uploads the LFS objects newly reachable from each
/// `<local-sha>`.
#[derive(Args)]
pub struct PrePushArgs {
    /// Name of the remote being pushed to.
    pub remote: String,
    /// URL of the remote (informational; we use `lfs.url` config).
    pub url: Option<String>,
    /// List the objects that would be pushed without actually
    /// uploading them.
    #[arg(long)]
    pub dry_run: bool,
}

/// Print the git-lfs version and exit.
#[derive(Args)]
pub struct VersionArgs;

/// Debug helper: build a pointer from a file, parse one from disk
/// or stdin, or just check whether some bytes are a valid pointer.
#[derive(Args)]
pub struct PointerArgs {
    // TODO(post-1.0): replace the --strict/--no-strict, --check/--pointer,
    // and --check/--file/--stdin manual checks (cli/src/pointer_cmd.rs:108,
    // 218, 223, 230, 241) with clap arg_group/conflicts_with/requires.
    // No shell test asserts this wording, so the constraint here is
    // softer than for the other commands — the deferral is purely about
    // upstream parity. Worth taking whenever.
    /// Build a pointer from this file (read content, hash, encode).
    #[arg(short, long)]
    pub file: Option<PathBuf>,
    /// Parse and display this existing pointer file.
    #[arg(short, long)]
    pub pointer: Option<PathBuf>,
    /// Read a pointer from stdin (mutually exclusive with --pointer).
    #[arg(long)]
    pub stdin: bool,
    /// Validity check mode: exit 0 if input parses, 1 if not, 2 if
    /// `--strict` and not byte-canonical.
    #[arg(long)]
    pub check: bool,
    /// In `--check`, also reject non-canonical pointers.
    #[arg(long)]
    pub strict: bool,
    /// Explicitly disable strict mode (paired with `--strict`).
    #[arg(long)]
    pub no_strict: bool,
}

/// Show the LFS environment: version, endpoints, on-disk paths, and
/// the three `filter.lfs.*` config values.
#[derive(Args)]
pub struct EnvArgs;

/// List the configured LFS pointer extensions (`lfs.extension.<name>.*`).
/// Extensions chain external clean/smudge programs around each LFS
/// object; this prints their resolved configuration in priority order.
#[derive(Args)]
pub struct ExtArgs;

/// (Re-)install the four LFS git hooks (`pre-push`, `post-checkout`,
/// `post-commit`, `post-merge`) for the current repository.
#[derive(Args)]
pub struct UpdateArgs {
    /// Overwrite any custom hook contents.
    #[arg(long)]
    pub force: bool,
    /// Print install instructions instead of writing the hook files.
    #[arg(long)]
    pub manual: bool,
}

/// Analyze or rewrite history for LFS conversion. Phase 1 ships
/// `info` only; `import` and `export` will land in subsequent phases.
#[derive(Args)]
pub struct MigrateArgs {
    #[command(subcommand)]
    pub cmd: MigrateCmd,
}

#[derive(Subcommand)]
pub enum MigrateCmd {
    Import(MigrateImportArgs),
    Export(MigrateExportArgs),
    Info(MigrateInfoArgs),
}

/// Rewrite history so files matching the include filter become LFS
/// pointers. With `--no-rewrite`, history is preserved and one
/// new commit is appended on top of HEAD with the named paths
/// converted in place.
#[derive(Args)]
pub struct MigrateImportArgs {
    // TODO(post-1.0): replace the manual --no-rewrite/--fixup/--above/
    // --include/--exclude/--everything cross-flag validation
    // (cli/src/migrate/import.rs:53-77, plus the shared
    // --everything/positional check in migrate/mod.rs::resolve_refs)
    // with clap arg_group/conflicts_with. Currently kept as-is because
    // tests/t-migrate-fixup.sh:94,112,130 and t-migrate-import.sh:814,
    // 825,836 assert upstream's exact wording (e.g. "--no-rewrite and
    // --fixup cannot be combined", "Cannot use --everything with
    // --include-ref or --exclude-ref"). Worth taking once we're free
    // to update those assertions.
    /// Without `--no-rewrite`: branches/refs to rewrite (empty =
    /// current branch). With `--no-rewrite`: working-tree paths
    /// to convert.
    pub args: Vec<String>,
    /// Walk every local branch and tag.
    #[arg(long)]
    pub everything: bool,
    /// Convert paths matching this glob (repeatable). Required
    /// unless `--above` is set or `--no-rewrite` is given.
    #[arg(short = 'I', long = "include")]
    pub include: Vec<String>,
    /// Exclude paths matching this glob (repeatable).
    #[arg(short = 'X', long = "exclude")]
    pub exclude: Vec<String>,
    /// Restrict the rewrite to commits reachable from these refs.
    /// Repeatable.
    #[arg(long = "include-ref")]
    pub include_ref: Vec<String>,
    /// Exclude commits reachable from these refs. Repeatable.
    #[arg(long = "exclude-ref")]
    pub exclude_ref: Vec<String>,
    /// Only convert files at least this large (e.g. `1mb`,
    /// `500k`).
    #[arg(long, default_value = "")]
    pub above: String,
    /// Don't rewrite history. Read named paths from the working
    /// tree, convert in place, append one new commit on top of
    /// HEAD.
    #[arg(long)]
    pub no_rewrite: bool,
    /// Commit message for the `--no-rewrite` commit.
    #[arg(short, long)]
    pub message: Option<String>,
    /// Skip the prompt confirming history rewrite. Currently we
    /// never prompt, so this is accepted as a no-op for parity
    /// with upstream's CLI surface.
    #[arg(long)]
    pub yes: bool,
    /// Walk every commit and convert files that *should* be LFS
    /// pointers (per their commit's `.gitattributes`) but
    /// currently aren't. Mutually exclusive with `--include`,
    /// `--exclude`, `--no-rewrite`.
    #[arg(long)]
    pub fixup: bool,
    /// Don't fetch missing LFS objects from the remote before the
    /// rewrite — accepted as a no-op since we never auto-fetch
    /// today.
    #[arg(long)]
    pub skip_fetch: bool,
    /// Write a comma-separated `<old>,<new>` mapping of every
    /// rewritten commit OID to the named file.
    #[arg(long = "object-map")]
    pub object_map: Option<PathBuf>,
    /// Print a per-commit progress line as the rewrite walks
    /// history.
    #[arg(long)]
    pub verbose: bool,
    /// Remote to consult when fetching missing LFS objects (default
    /// `origin`).
    #[arg(long)]
    pub remote: Option<String>,
}

/// Inverse of import: rewrite history so LFS pointers become the
/// raw bytes they reference. Requires the LFS objects to already
/// be in the local store — `git lfs fetch` first if not. Pointers
/// whose objects are missing are left as-is.
#[derive(Args)]
pub struct MigrateExportArgs {
    // TODO(post-1.0): make --include a required clap arg (it is required
    // in practice — cli/src/migrate/export.rs:53). Currently kept as a
    // runtime check because tests/t-migrate-export.sh:208 asserts
    // upstream's exact wording ("One or more files must be specified
    // with --include"); clap's "the following required arguments were
    // not provided: --include <INCLUDE>" would be a strict UX win but
    // a behavioral diff. Also see the shared --everything/positional
    // check in migrate/mod.rs::resolve_refs.
    /// Branches / refs to rewrite. Empty = current branch.
    pub branches: Vec<String>,
    /// Walk every local branch and tag.
    #[arg(long)]
    pub everything: bool,
    /// Convert pointers at paths matching this glob (repeatable).
    /// Required.
    #[arg(short = 'I', long = "include")]
    pub include: Vec<String>,
    /// Don't convert pointers at paths matching this glob.
    #[arg(short = 'X', long = "exclude")]
    pub exclude: Vec<String>,
    /// Restrict the rewrite to commits reachable from these refs.
    /// Repeatable.
    #[arg(long = "include-ref")]
    pub include_ref: Vec<String>,
    /// Exclude commits reachable from these refs. Repeatable.
    #[arg(long = "exclude-ref")]
    pub exclude_ref: Vec<String>,
    /// Don't fetch missing LFS objects from the remote before the
    /// rewrite — leave their pointers in place.
    #[arg(long)]
    pub skip_fetch: bool,
    /// Write a comma-separated `<old>,<new>` mapping of every
    /// rewritten commit OID to the named file. Useful as input to
    /// `git filter-repo` or other downstream tools.
    #[arg(long = "object-map")]
    pub object_map: Option<PathBuf>,
    /// Print a per-commit progress line as the rewrite walks
    /// history.
    #[arg(long)]
    pub verbose: bool,
    /// Remote to consult when fetching missing LFS objects (default
    /// `origin`).
    #[arg(long)]
    pub remote: Option<String>,
    /// Skip the prompt confirming history rewrite. Currently we
    /// never prompt, so this is accepted as a no-op for parity
    /// with upstream's CLI surface.
    #[arg(long)]
    pub yes: bool,
}

/// Walk history and report file extensions by total size.
/// Read-only — no objects or history change.
#[derive(Args)]
pub struct MigrateInfoArgs {
    // TODO(post-1.0): replace the manual --everything/--include-ref/
    // --exclude-ref/--fixup/--pointers/--include/--exclude cross-flag
    // validation (cli/src/migrate/info.rs:59-83, plus the shared
    // --everything/positional check in migrate/mod.rs::resolve_refs)
    // with clap arg_group/conflicts_with. Currently kept as-is because
    // tests/t-migrate-info.sh:903,922,941,977,995,1013,1031 assert
    // upstream's exact wording (e.g. "Cannot use --fixup with
    // --pointers=follow"). The value-conditional --pointers checks
    // ("=follow" / "=no-follow") may not all collapse cleanly to
    // declarative clap rules.
    /// Branches / refs to scan. Empty = current branch.
    pub branches: Vec<String>,
    /// Walk every local branch and tag.
    #[arg(long)]
    pub everything: bool,
    /// Only include paths matching this glob (repeatable).
    #[arg(short = 'I', long = "include")]
    pub include: Vec<String>,
    /// Exclude paths matching this glob (repeatable).
    #[arg(short = 'X', long = "exclude")]
    pub exclude: Vec<String>,
    /// Restrict the scan to commits reachable from these refs.
    /// Repeatable.
    #[arg(long = "include-ref")]
    pub include_ref: Vec<String>,
    /// Exclude commits reachable from these refs. Repeatable.
    #[arg(long = "exclude-ref")]
    pub exclude_ref: Vec<String>,
    /// Only count files at least this large (e.g. `1mb`, `500k`).
    #[arg(long, default_value = "")]
    pub above: String,
    /// Maximum extension rows to show.
    #[arg(long, default_value_t = 5)]
    pub top: usize,
    /// How to handle existing LFS pointer blobs:
    /// `follow` (default), `ignore`, or `no-follow`. Defaults
    /// based on `--fixup`: `ignore` with the flag, `follow`
    /// without.
    #[arg(long)]
    pub pointers: Option<String>,
    /// Force the size unit for byte counts (`b`, `kb`, `mb`,
    /// `gb`, `tb`, `pb`). Auto-scaled when omitted.
    #[arg(long)]
    pub unit: Option<String>,
    /// Don't fetch missing LFS objects from the remote — accepted
    /// as a no-op since we don't auto-fetch today.
    #[arg(long)]
    pub skip_fetch: bool,
    /// Remote to consult (no-op for now; reserved for the
    /// auto-fetch path).
    #[arg(long)]
    pub remote: Option<String>,
    /// Walk history looking for files that *should* be LFS but
    /// aren't (per `.gitattributes`). Implies `--pointers=ignore`.
    #[arg(long)]
    pub fixup: bool,
}

/// Populate working copy with real content from Git LFS files.
///
/// Try to ensure that the working copy contains file content for Git LFS
/// objects for the current ref, if the object data is available. Does not
/// download any content; see git-lfs-fetch(1) for that.
///
/// Checkout scans the current ref for all LFS objects that would be
/// required, then where a file is either missing in the working copy, or
/// contains placeholder pointer content with the same SHA, the real file
/// content is written, provided we have it in the local store. Modified
/// files are never overwritten.
///
/// One or more may be provided as arguments to restrict the set of files
/// that are updated. Glob patterns are matched as per the format described
/// in gitignore(5).
///
/// When used with `--to` and the working tree is in a conflicted state due
/// to a merge, this option checks out one of the three stages a conflicting
/// Git LFS object into a separate file (which can be outside of the work
/// tree). This can make using diff tools to inspect and resolve merges
/// easier. A single Git LFS object's file path must be provided in
/// `PATHS`. If `FILE` already exists, whether as a regular
/// file, symbolic link, or directory, it will be removed and replaced, unless
/// it is a non-empty directory or otherwise cannot be deleted.
///
/// If the installed Git version is at least 2.42.0,
/// this command will by default check out Git LFS objects for files
/// only if they are present in the Git index and if they match a Git LFS
/// filter attribute from a `.gitattributes` file that is present in either
/// the index or the current working tree (or, as is always the case, if
/// they match a Git LFS filter attribute in a local gitattributes file
/// such as `$GIT_DIR/info/attributes`). These constraints do not apply
/// with prior versions of Git.
///
/// In a repository with a partial clone or sparse checkout, it is therefore
/// advisable to check out all `.gitattributes` files from HEAD before
/// using this command, if Git v2.42.0 or later is installed. Alternatively,
/// the `GIT_ATTR_SOURCE` environment variable may be set to HEAD, which
/// will cause Git to only read attributes from `.gitattributes` files in
/// HEAD and ignore those in the index or working tree.
///
/// In a bare repository, this command prints an informational message and
/// exits without modifying anything. In a future version, it may exit with
/// an error.
#[derive(Args)]
pub struct CheckoutArgs {
    // TODO(post-1.0): replace this manual stage/--to validation with
    // clap arg_group/requires/conflicts_with. Currently kept as-is
    // because tests/t-checkout.sh:897-909 assert upstream's exact error
    // wording; clap's wording would be a strict UX improvement but a
    // behavioral diff. Worth taking once we're free to update those
    // assertions.
    /// Check out the merge base of the specified file
    #[arg(long)]
    pub base: bool,

    /// Check out our side (that of the current branch) of the
    /// conflict for the specified file
    #[arg(long)]
    pub ours: bool,

    /// Check out their side (that of the other branch) of the
    /// conflict for the specified file
    #[arg(long)]
    pub theirs: bool,

    /// If the working tree is in a conflicted state, check out the
    /// portion of the conflict specified by `--base`, `--ours`, or
    /// `--theirs` to the given path. Exactly one of these options
    /// is required.
    #[arg(long, value_name = "FILE")]
    pub to: Option<String>,

    /// Paths to check out.
    ///
    /// When empty, everything in HEAD's tree is checked out. In
    /// conflict mode (`--to <path>` together with one of `--base`,
    /// `--ours`, or `--theirs`), exactly one path is required.
    pub paths: Vec<String>,
}

/// Delete local LFS objects that aren't reachable from HEAD or any
/// unpushed commit. Reclaims disk for repos whose history has moved
/// past their objects.
#[derive(Args)]
pub struct PruneArgs {
    /// Don't delete anything; just report what would go.
    #[arg(short, long)]
    pub dry_run: bool,
    /// Print each prunable object's OID and size.
    #[arg(short, long)]
    pub verbose: bool,
}

/// Check the integrity of LFS objects and pointers reachable from
/// `<refspec>` (default: HEAD). Exit 1 if anything is corrupt.
#[derive(Args)]
pub struct FsckArgs {
    /// Ref to scan. Defaults to HEAD.
    pub refspec: Option<String>,
    /// Only check objects (verify store contents match pointer OIDs).
    #[arg(long)]
    pub objects: bool,
    /// Only check pointers (flag non-canonical pointer encodings).
    #[arg(long)]
    pub pointers: bool,
    /// Report problems but don't move corrupt objects to `<lfs>/bad/`.
    #[arg(short, long)]
    pub dry_run: bool,
}

/// Show staged + unstaged changes, classifying each blob as LFS,
/// Git, or working-tree File.
#[derive(Args)]
pub struct StatusArgs {
    /// Stable one-line-per-change format for scripts.
    #[arg(short, long)]
    pub porcelain: bool,
    /// Stable JSON output for scripts; only LFS entries are reported.
    #[arg(short, long)]
    pub json: bool,
}

/// Acquire an exclusive server-side lock on one or more files.
/// Other users will be unable to push changes to a locked file.
#[derive(Args)]
pub struct LockArgs {
    /// Paths to lock (repo-relative or absolute, must resolve inside
    /// the working tree).
    pub paths: Vec<String>,
    /// Specify which remote to use when interacting with locks.
    #[arg(short, long)]
    pub remote: Option<String>,
    /// Refspec to associate the lock with. Defaults to the current
    /// branch's tracked upstream (`branch.<current>.merge`) or the
    /// current branch's full ref (`refs/heads/<branch>`).
    #[arg(long = "ref")]
    pub refspec: Option<String>,
    /// Stable JSON output for scripts.
    #[arg(short, long)]
    pub json: bool,
}

/// List file locks held on the server.
#[derive(Args)]
pub struct LocksArgs {
    /// Specify which remote to use when interacting with locks.
    #[arg(short, long)]
    pub remote: Option<String>,
    /// Filter results to a particular path.
    #[arg(short, long)]
    pub path: Option<String>,
    /// Filter results to a particular lock id.
    #[arg(short, long)]
    pub id: Option<String>,
    /// Maximum number of results to return.
    #[arg(short, long)]
    pub limit: Option<u32>,
    /// Refspec to filter locks by (defaults to current branch /
    /// tracked upstream — same auto-resolution as `git lfs lock`).
    #[arg(long = "ref")]
    pub refspec: Option<String>,
    /// Verify ownership: prefix locks owned by the authenticated user
    /// with `O ` (others get `  `).
    #[arg(long)]
    pub verify: bool,
    /// List from the on-disk cache of own locks instead of querying
    /// the server. Combine with `--path` / `--id` / `--limit` to
    /// filter; `--verify` is rejected. Useful when offline or to
    /// confirm what `git lfs lock` recorded locally.
    #[arg(long)]
    pub local: bool,
    /// Stable JSON output for scripts.
    #[arg(short, long)]
    pub json: bool,
}

/// Release a file lock previously acquired with `git lfs lock`.
/// Either provide one or more paths, or `--id <id>` (mutually
/// exclusive).
#[derive(Args)]
pub struct UnlockArgs {
    // TODO(post-1.0): replace the manual --id-xor-paths check
    // (cli/src/lock.rs:301-306) with a clap ArgGroup
    // (required = true, multiple = false) covering --id and the
    // positional paths arg. Currently kept as-is because
    // tests/t-unlock.sh:228,431,482 assert upstream's exact wording
    // ("Exactly one of --id or a set of paths must be provided").
    // Worth taking once we're free to update those assertions.
    /// Paths to unlock; mutually exclusive with `--id`.
    pub paths: Vec<String>,
    /// Lock id to release; mutually exclusive with paths.
    #[arg(short, long)]
    pub id: Option<String>,
    /// Forcibly break another user's lock(s).
    #[arg(short, long)]
    pub force: bool,
    /// Specify which remote to use when interacting with locks.
    #[arg(short, long)]
    pub remote: Option<String>,
    /// Refspec to send with the unlock request (defaults to current
    /// branch / tracked upstream).
    #[arg(long = "ref")]
    pub refspec: Option<String>,
    /// Stable JSON output for scripts.
    #[arg(short, long)]
    pub json: bool,
}

/// List LFS-tracked files visible at a ref (default: HEAD), or across
/// all reachable history with `--all`.
#[derive(Args)]
pub struct LsFilesArgs {
    /// Ref to list. Defaults to HEAD.
    pub refspec: Option<String>,
    /// Show full 64-char OID instead of the 10-char prefix.
    #[arg(short, long)]
    pub long: bool,
    /// Append humanized size in parens.
    #[arg(short, long)]
    pub size: bool,
    /// Print only the path.
    #[arg(short, long)]
    pub name_only: bool,
    /// Walk every reachable ref's full history.
    #[arg(short, long)]
    pub all: bool,
    /// Multi-line per-file block (size, checkout, download, oid, version).
    #[arg(short, long)]
    pub debug: bool,
    /// Stable JSON output for scripts.
    #[arg(short, long)]
    pub json: bool,
}
