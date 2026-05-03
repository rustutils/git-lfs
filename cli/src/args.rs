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

/// Install Git LFS configuration
///
/// Set up the `lfs` smudge and clean filters under the name `lfs` in
/// the global Git config, and (when run from inside a repository)
/// install a pre-push hook to run git-lfs-pre-push(1). If
/// `core.hooksPath` is configured in any Git configuration (supported
/// on Git v2.9.0 or later), the pre-push hook is installed to that
/// directory instead.
///
/// Without any options, only sets up the `lfs` smudge and clean filters
/// if they are not already set.
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
    /// Set the `lfs` smudge and clean filters, overwriting existing
    /// values.
    #[arg(short, long)]
    pub force: bool,

    /// Set the `lfs` smudge and clean filters in the local repository's
    /// git config, instead of the global git config (`~/.gitconfig`).
    #[arg(short, long)]
    pub local: bool,

    /// Set the `lfs` smudge and clean filters in the current working
    /// tree's git config, instead of the global git config
    /// (`~/.gitconfig`) or local repository's git config
    /// (`$GIT_DIR/config`).
    ///
    /// If multiple working trees are in use, the Git config extension
    /// `worktreeConfig` must be enabled to use this option. If only one
    /// working tree is in use, `--worktree` has the same effect as
    /// `--local`. Available only on Git v2.20.0 or later.
    #[arg(short, long)]
    pub worktree: bool,

    /// Set the `lfs` smudge and clean filters in the system git config,
    /// e.g. `/etc/gitconfig` instead of the global git config
    /// (`~/.gitconfig`).
    #[arg(long)]
    pub system: bool,

    /// Set the `lfs` smudge and clean filters in the Git configuration
    /// file specified by `<PATH>`.
    #[arg(long, value_name = "PATH")]
    pub file: Option<PathBuf>,

    /// Skip automatic downloading of objects on clone or pull.
    ///
    /// Requires a manual `git lfs pull` every time a new commit is
    /// checked out on the repository.
    #[arg(short, long)]
    pub skip_smudge: bool,

    /// Skip installation of hooks into the local repository.
    ///
    /// Use if you want to install the LFS filters but not make changes
    /// to the hooks. Valid alongside `--local`, `--worktree`, `--system`,
    /// or `--file`.
    #[arg(long)]
    pub skip_repo: bool,
}

/// Remove Git LFS configuration
///
/// Remove the `lfs` clean and smudge filters from the global Git config,
/// and (when run from inside a Git repository) uninstall the Git LFS
/// pre-push hook. Hooks that don't match what we would write are left
/// untouched.
#[derive(Args)]
pub struct UninstallArgs {
    // TODO(post-1.0): same --local/--system/--worktree/--file mutex as
    // InstallArgs — share a clap ArgGroup. See InstallArgs's TODO for
    // the rationale and test references.
    /// Optional mode. With `hooks`, removes only the LFS git hooks and
    /// leaves the filter config alone (the inverse of `--skip-repo`).
    pub mode: Option<String>,

    /// Remove the `lfs` smudge and clean filters from the local
    /// repository's git config, instead of the global git config
    /// (`~/.gitconfig`).
    #[arg(short, long)]
    pub local: bool,

    /// Remove the `lfs` smudge and clean filters from the current
    /// working tree's git config, instead of the global git config
    /// (`~/.gitconfig`) or local repository's git config
    /// (`$GIT_DIR/config`).
    ///
    /// If multiple working trees are in use, the Git config extension
    /// `worktreeConfig` must be enabled to use this option. If only one
    /// working tree is in use, `--worktree` has the same effect as
    /// `--local`. Available only on Git v2.20.0 or later.
    #[arg(short, long)]
    pub worktree: bool,

    /// Remove the `lfs` smudge and clean filters from the system git
    /// config, instead of the global git config (`~/.gitconfig`).
    #[arg(long)]
    pub system: bool,

    /// Remove the `lfs` smudge and clean filters from the Git
    /// configuration file specified by `<PATH>`.
    #[arg(long, value_name = "PATH")]
    pub file: Option<PathBuf>,

    /// Skip cleanup of the local repo.
    ///
    /// Use if you want to uninstall the global LFS filters but not
    /// make changes to the current repo.
    #[arg(long)]
    pub skip_repo: bool,
}

/// View or add Git LFS paths to Git attributes
///
/// Start tracking the given pattern(s) through Git LFS. The argument is
/// written to `.gitattributes`. If no paths are provided, list the
/// currently-tracked paths.
///
/// Per gitattributes(5), patterns use the gitignore(5) pattern rules to
/// match paths. This means that patterns containing asterisk (`*`),
/// question mark (`?`), and the bracket characters (`[` and `]`) are
/// treated specially; to disable this behavior and treat them literally
/// instead, use `--filename` or escape the character with a backslash.
#[derive(Args)]
pub struct TrackArgs {
    /// File patterns to track (e.g. `*.jpg`, `data/*.bin`).
    pub patterns: Vec<String>,

    /// Log files which `git lfs track` will touch. Disabled by default.
    #[arg(short, long)]
    pub verbose: bool,

    /// Log all actions that would normally take place (adding entries
    /// to `.gitattributes`, touching files on disk, etc.) without
    /// performing any mutative operations.
    ///
    /// Implicitly mocks the behavior of `--verbose`, logging in greater
    /// detail what it is doing. Disabled by default.
    #[arg(short, long)]
    pub dry_run: bool,

    /// Write the currently tracked patterns as JSON to standard output.
    ///
    /// Intended for interoperation with external tools. Cannot be
    /// combined with any pattern arguments. If `--no-excluded` is also
    /// provided, that option will have no effect.
    #[arg(short, long)]
    pub json: bool,

    /// Treat the arguments as literal filenames, not as patterns.
    ///
    /// Any special glob characters in the filename will be escaped
    /// when writing the `.gitattributes` file.
    #[arg(long)]
    pub filename: bool,

    /// Make the paths "lockable" — they should be locked to edit them,
    /// and will be made read-only in the working copy when not locked.
    #[arg(short, long)]
    pub lockable: bool,

    /// Remove the lockable flag from the paths so they are no longer
    /// read-only unless locked.
    #[arg(long)]
    pub not_lockable: bool,

    /// Don't list patterns that are excluded in the output; only list
    /// patterns that are tracked.
    #[arg(long)]
    pub no_excluded: bool,

    /// Make matched entries stat-dirty so that Git can re-index files
    /// you wish to convert to LFS.
    ///
    /// Does not modify any `.gitattributes` file.
    #[arg(long)]
    pub no_modify_attrs: bool,
}

/// Remove Git LFS paths from Git attributes
///
/// Stop tracking the given path(s) through Git LFS. The argument can
/// be a glob pattern or a file path. The matching pointer files in
/// history (and the objects in the local store) are left in place.
#[derive(Args)]
pub struct UntrackArgs {
    /// Paths or glob patterns to stop tracking.
    pub patterns: Vec<String>,
}

/// Git filter process that converts between pointer and actual content
///
/// Implement the Git process filter API, exchanging handshake messages
/// and then accepting and responding to requests to either clean or
/// smudge a file.
///
/// `filter-process` is always run by Git's filter process, and is
/// configured by the repository's Git attributes.
///
/// In your Git configuration or in a `.lfsconfig` file, you may set
/// either or both of `lfs.fetchinclude` and `lfs.fetchexclude` to
/// comma-separated lists of paths. If `lfs.fetchinclude` is defined,
/// Git LFS pointer files will only be replaced with the contents of
/// the corresponding object file if their path matches one in that
/// list, and if `lfs.fetchexclude` is defined, pointer files will
/// only be replaced if their path does not match one in that list.
/// Paths are matched using wildcard matching as per gitignore(5).
/// Pointer files that are not replaced are simply copied to standard
/// output without change.
///
/// The filter process uses Git's pkt-line protocol to communicate, and
/// is documented in detail in gitattributes(5).
#[derive(Args)]
pub struct FilterProcessArgs {
    /// Skip automatic downloading of objects on clone or pull.
    ///
    /// Equivalent to `GIT_LFS_SKIP_SMUDGE=1`. Wired up by
    /// `git lfs install --skip-smudge`.
    #[arg(short, long)]
    pub skip: bool,
}

/// Download all Git LFS files for a given ref
///
/// Download Git LFS objects at the given refs from the specified remote.
/// See DEFAULT REMOTE and DEFAULT REFS for what happens if you don't
/// specify.
///
/// This does not update the working copy; use git-lfs-pull(1) to
/// download and replace pointer text with object content, or
/// git-lfs-checkout(1) to materialize already-downloaded objects.
#[derive(Args)]
pub struct FetchArgs {
    /// Optional remote name followed by refs. The first positional
    /// argument is treated as a remote name when it resolves; any
    /// following arguments are refs to fetch.
    pub args: Vec<String>,

    /// Specify `lfs.fetchinclude` just for this invocation; see
    /// INCLUDE AND EXCLUDE.
    #[arg(short = 'I', long, help_heading = FILTER)]
    pub include: Vec<String>,

    /// Specify `lfs.fetchexclude` just for this invocation; see
    /// INCLUDE AND EXCLUDE.
    #[arg(short = 'X', long, help_heading = FILTER)]
    pub exclude: Vec<String>,

    /// Download all objects that are referenced by any commit
    /// reachable from the refs provided as arguments.
    ///
    /// If no refs are provided, then all refs are fetched. This is
    /// primarily for backup and migration purposes. Cannot be
    /// combined with `--include`/`--exclude`. Ignores any globally
    /// configured include and exclude paths to ensure that all
    /// objects are downloaded.
    #[arg(short, long)]
    pub all: bool,

    /// Read a list of newline-delimited refs from standard input
    /// instead of the command line.
    #[arg(long)]
    pub stdin: bool,

    /// Prune old and unreferenced objects after fetching, equivalent
    /// to running `git lfs prune` afterwards. See git-lfs-prune(1)
    /// for more details.
    #[arg(short, long)]
    pub prune: bool,

    /// Also fetch objects that are already present locally.
    ///
    /// Useful for recovery from a corrupt local store.
    #[arg(long)]
    pub refetch: bool,

    /// Print what would be fetched, without actually fetching anything.
    #[arg(short, long)]
    pub dry_run: bool,

    /// Write the details of all object transfer requests as JSON to
    /// standard output.
    ///
    /// Intended for interoperation with external tools. When
    /// `--dry-run` is also specified, writes the details of the
    /// transfers that would occur if the objects were fetched.
    #[arg(short, long)]
    pub json: bool,
}

const FILTER: &str = "Filter options";

/// Download all Git LFS files for current ref and checkout
///
/// Download Git LFS objects for the currently checked out ref, and
/// update the working copy with the downloaded content if required.
///
/// This is generally equivalent to running `git lfs fetch [options]
/// [<remote>]` followed by `git lfs checkout`. See git-lfs-checkout(1)
/// for partial-clone, sparse-checkout, and bare-repository behavior
/// (governed by the installed Git version and `GIT_ATTR_SOURCE`).
///
/// Requires `git lfs install` to have wired up the smudge filter. If
/// the filter is missing, the fetch step still runs but the
/// working-tree update is skipped with a hint to install.
#[derive(Args)]
pub struct PullArgs {
    /// Optional remote name followed by refs.
    ///
    /// The first positional argument is treated as a remote name when
    /// it resolves; any following arguments are refs to fetch. With
    /// no arguments, the default remote is used.
    pub args: Vec<String>,

    /// Specify `lfs.fetchinclude` just for this invocation.
    #[arg(short = 'I', long, help_heading = FILTER)]
    pub include: Vec<String>,

    /// Specify `lfs.fetchexclude` just for this invocation.
    #[arg(short = 'X', long, help_heading = FILTER)]
    pub exclude: Vec<String>,
}

/// Push queued large files to the Git LFS endpoint
///
/// Upload Git LFS files to the configured endpoint for the current Git
/// remote. By default, filters out objects that are already referenced
/// by the local clone of the remote (approximated via
/// `refs/remotes/<remote>/*`); the server's batch API dedupes again,
/// so a missing local tracking ref doesn't waste bandwidth.
#[derive(Args)]
pub struct PushArgs {
    /// Remote to push to (e.g. `origin`). The remote's tracking refs
    /// are excluded from the upload set so already-pushed objects
    /// aren't sent again.
    pub remote: String,

    /// Refs (or, with `--object-id`, raw OIDs) to push. With `--all`,
    /// restricts the all-refs walk to these; with `--stdin`, ignored
    /// (a warning is emitted).
    pub args: Vec<String>,

    /// Print the files that would be pushed, without actually pushing
    /// them.
    #[arg(short, long)]
    pub dry_run: bool,

    /// Push all objects reachable from the refs given as arguments.
    ///
    /// If no refs are provided, all local refs are pushed. Note this
    /// behavior differs from `git lfs fetch --all`, which fetches
    /// every ref including refs outside `refs/heads` / `refs/tags`. If
    /// you're migrating a repository, run `git lfs push` for any
    /// additional remote refs that contain LFS objects not reachable
    /// from your local refs.
    #[arg(short, long)]
    pub all: bool,

    /// Push only the object OIDs listed on the command line (or read
    /// from stdin with `--stdin`), separated by spaces.
    #[arg(short, long)]
    pub object_id: bool,

    /// Read newline-delimited refs (or object IDs when using
    /// `--object-id`) from standard input instead of the command
    /// line.
    #[arg(long)]
    pub stdin: bool,
}

/// Efficiently clone a LFS-enabled repository
///
/// Clone an LFS-enabled Git repository by disabling LFS during the
/// `git clone`, then running `git lfs pull` directly afterwards.
/// Also installs the repo-level hooks (`.git/hooks`) that LFS requires
/// to operate; if `--separate-git-dir` is given to `git clone`, the
/// hooks are installed there.
///
/// Historically faster than a regular `git clone` because that would
/// download LFS content via the smudge filter one file at a time.
/// Modern `git clone` parallelizes the smudge filter, so this command
/// no longer offers a meaningful speedup over plain `git clone`. You
/// should prefer plain `git clone`.
///
/// In addition to the options accepted by `git clone`, the LFS-only
/// flags `--include` / `-I <paths>`, `--exclude` / `-X <paths>`, and
/// `--skip-repo` (skip installing the repo-level hooks) are accepted
/// — see git-lfs-fetch(1) for the include/exclude semantics. They're
/// parsed from the trailing argument list rather than declared as
/// clap flags, so they don't appear in this command's `--help`.
#[derive(Args)]
pub struct CloneArgs {
    /// `git clone` arguments plus the LFS pass-through flags
    /// (`-I`/`--include`, `-X`/`--exclude`, `--skip-repo`). The
    /// repository URL is required; an optional target directory
    /// follows.
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

/// Print the git-lfs version banner and exit
#[derive(Args)]
pub struct VersionArgs;

/// Build, compare, and check pointers
///
/// Build and optionally compare generated pointer files to ensure
/// consistency between different Git LFS implementations.
#[derive(Args)]
pub struct PointerArgs {
    // TODO(post-1.0): replace the --strict/--no-strict, --check/--pointer,
    // and --check/--file/--stdin manual checks (cli/src/pointer_cmd.rs:108,
    // 218, 223, 230, 241) with clap arg_group/conflicts_with/requires.
    // No shell test asserts this wording, so the constraint here is
    // softer than for the other commands — the deferral is purely about
    // upstream parity. Worth taking whenever.
    /// A local file to build the pointer from.
    #[arg(short, long)]
    pub file: Option<PathBuf>,

    /// A local file containing a pointer generated from another
    /// implementation.
    ///
    /// Compared to the pointer generated from `--file`.
    #[arg(short, long)]
    pub pointer: Option<PathBuf>,

    /// Read the pointer from standard input to compare with the
    /// pointer generated from `--file`.
    #[arg(long)]
    pub stdin: bool,

    /// Read the pointer from standard input (with `--stdin`) or the
    /// filepath (with `--file`).
    ///
    /// If neither or both of `--stdin` and `--file` are given, the
    /// invocation is invalid. Exits 0 if the data read is a valid Git
    /// LFS pointer, 1 otherwise. With `--strict`, exits 2 if the
    /// pointer is not byte-canonical.
    #[arg(long)]
    pub check: bool,

    /// With `--check`, verify that the pointer is canonical (the one
    /// Git LFS would create).
    ///
    /// If it isn't, exits 2. The default — for backwards compatibility
    /// — is `--no-strict`.
    #[arg(long)]
    pub strict: bool,

    /// Disable strict mode (paired with `--strict`).
    #[arg(long)]
    pub no_strict: bool,
}

/// Display the Git LFS environment
///
/// Display the current Git LFS environment: version, endpoints,
/// on-disk paths, and the three `filter.lfs.*` config values.
#[derive(Args)]
pub struct EnvArgs;

/// List the configured LFS pointer extensions
///
/// Print each `lfs.extension.<name>.*` entry resolved to its final
/// configuration in priority order. Extensions chain external
/// clean / smudge programs around each LFS object — see
/// git-lfs-config(5) for how to configure them.
#[derive(Args)]
pub struct ExtArgs;

/// Update Git hooks
///
/// Update the Git hooks used by Git LFS. Silently upgrades known hook
/// contents. If you have your own custom hooks you may need to use
/// one of the extended options below.
#[derive(Args)]
pub struct UpdateArgs {
    /// Forcibly overwrite any existing hooks with git-lfs hooks.
    ///
    /// Use this option if `git lfs update` fails because of existing
    /// hooks but you don't care about their current contents.
    #[arg(short, long)]
    pub force: bool,

    /// Print instructions for manually updating your hooks to
    /// include git-lfs functionality.
    ///
    /// Use this option if `git lfs update` fails because of existing
    /// hooks and you want to retain their functionality.
    #[arg(short, long)]
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

/// Delete old LFS files from local storage
///
/// Delete locally stored LFS objects that aren't reachable from HEAD
/// or any unpushed commit, freeing up disk space.
///
/// Note: many of upstream's prune options aren't yet supported —
/// `--force`, `--recent`, `--verify-remote` (and the `--no-...`
/// variants), `--verify-unreachable`, `--when-unverified`, the
/// recent-refs / recent-commits retention windows, and the
/// stash / worktree retention rules. The basic
/// reachable-from-HEAD-or-unpushed walk is implemented and matches
/// upstream's default semantics.
#[derive(Args)]
pub struct PruneArgs {
    /// Don't actually delete anything; just report what would have
    /// been done.
    #[arg(short, long)]
    pub dry_run: bool,

    /// Report the full detail of what is/would be deleted.
    #[arg(short, long)]
    pub verbose: bool,
}

/// Check Git LFS files for consistency
///
/// Check all Git LFS files in the current HEAD for consistency.
/// Corrupted files are moved to `.git/lfs/bad`.
///
/// A single committish may be given to inspect that commit instead of
/// HEAD. The `<a>..<b>` range form from upstream is not yet supported
/// — only a single ref is accepted. With no argument, HEAD is
/// examined.
///
/// The default is to perform all checks. `lfs.fetchexclude` is also
/// not yet honored on this command; objects whose paths match the
/// exclude list will still be checked.
#[derive(Args)]
pub struct FsckArgs {
    /// Ref to scan. Defaults to HEAD.
    pub refspec: Option<String>,

    /// Check that each object in HEAD matches its expected hash and
    /// that each object exists on disk.
    #[arg(long)]
    pub objects: bool,

    /// Check that each pointer is canonical and that each file which
    /// should be stored as a Git LFS file is so stored.
    #[arg(long)]
    pub pointers: bool,

    /// Perform checks, but do not move any corrupted files to
    /// `.git/lfs/bad`.
    #[arg(short, long)]
    pub dry_run: bool,
}

/// Show the status of Git LFS files in the working tree
///
/// Display paths of Git LFS objects that have not been pushed to the
/// Git LFS server (large files that would be uploaded by `git push`),
/// that have differences between the index file and the current HEAD
/// commit (large files that would be committed by `git commit`), or
/// that have differences between the working tree and the index file
/// (files that could be staged with `git add`).
///
/// Must be run in a non-bare repository.
#[derive(Args)]
pub struct StatusArgs {
    /// Give the output in an easy-to-parse format for scripts.
    #[arg(short, long)]
    pub porcelain: bool,

    /// Write Git LFS file status information as JSON to standard
    /// output if the command exits successfully.
    ///
    /// Intended for interoperation with external tools. If
    /// `--porcelain` is also provided, that option takes precedence.
    #[arg(short, long)]
    pub json: bool,
}

/// Set a file as "locked" on the Git LFS server
///
/// Sets the given file path as "locked" against the Git LFS server,
/// with the intention of blocking attempts by other users to update
/// the given path. Locking a file requires the file to exist in the
/// working copy.
///
/// Once locked, LFS will verify that Git pushes do not modify files
/// locked by other users. See the description of the
/// `lfs.<url>.locksverify` config key in git-lfs-config(5) for
/// details.
#[derive(Args)]
pub struct LockArgs {
    /// Paths to lock. Repo-relative or absolute; must resolve inside
    /// the working tree. Upstream's CLI accepts a single path; ours
    /// accepts multiple (additive extension).
    pub paths: Vec<String>,

    /// Specify the Git LFS server to use. Ignored if the `lfs.url`
    /// config key is set.
    #[arg(short, long)]
    pub remote: Option<String>,

    /// Write lock info as JSON to standard output if the command
    /// exits successfully.
    ///
    /// Intended for interoperation with external tools. If the command
    /// returns with a non-zero exit code, plain text messages are sent
    /// to standard error.
    #[arg(short, long)]
    pub json: bool,

    /// Refspec to associate the lock with (extension over upstream).
    ///
    /// Defaults to the current branch's tracked upstream
    /// (`branch.<current>.merge`) or the current branch's full ref
    /// (`refs/heads/<branch>`).
    #[arg(long = "ref")]
    pub refspec: Option<String>,
}

/// Lists currently locked files from the Git LFS server
///
/// Lists current locks from the Git LFS server. Without filters, all
/// locks visible to the configured remote are returned.
#[derive(Args)]
pub struct LocksArgs {
    /// Specify the Git LFS server to use. Ignored if the `lfs.url`
    /// config key is set.
    #[arg(short, long)]
    pub remote: Option<String>,

    /// Specify a lock by its ID. Returns a single result.
    #[arg(short, long)]
    pub id: Option<String>,

    /// Specify a lock by its path. Returns a single result.
    #[arg(short, long)]
    pub path: Option<String>,

    /// List only our own locks which are cached locally. Skips a
    /// remote call.
    ///
    /// Useful when offline or to confirm what `git lfs lock` recorded
    /// locally. Combine with `--path` / `--id` / `--limit` to filter;
    /// `--verify` is rejected.
    #[arg(long)]
    pub local: bool,

    /// Verify the lock owner on the server and mark our own locks
    /// with `O`.
    ///
    /// Own locks are held by us and the corresponding files can be
    /// updated for the next push. All other locks are held by someone
    /// else. Contrary to `--local`, this also detects locks held by us
    /// despite no local lock information being available (e.g. because
    /// the file had been locked from a different clone) and detects
    /// "broken" locks (e.g. someone else forcibly unlocked our files).
    #[arg(long)]
    pub verify: bool,

    /// Maximum number of results to return.
    #[arg(short, long)]
    pub limit: Option<u32>,

    /// Write lock info as JSON to standard output if the command
    /// exits successfully.
    ///
    /// Intended for interoperation with external tools. If the command
    /// returns with a non-zero exit code, plain text messages are sent
    /// to standard error.
    #[arg(short, long)]
    pub json: bool,

    /// Refspec to filter locks by (extension over upstream).
    ///
    /// Defaults to the current branch's tracked upstream — same
    /// auto-resolution as `git lfs lock`.
    #[arg(long = "ref")]
    pub refspec: Option<String>,
}

/// Remove "locked" setting for a file on the Git LFS server
///
/// Removes the given file path as "locked" on the Git LFS server.
/// Files must exist and have a clean git status before they can be
/// unlocked. The `--force` flag will skip these checks.
#[derive(Args)]
pub struct UnlockArgs {
    // TODO(post-1.0): replace the manual --id-xor-paths check
    // (cli/src/lock.rs:301-306) with a clap ArgGroup
    // (required = true, multiple = false) covering --id and the
    // positional paths arg. Currently kept as-is because
    // tests/t-unlock.sh:228,431,482 assert upstream's exact wording
    // ("Exactly one of --id or a set of paths must be provided").
    // Worth taking once we're free to update those assertions.
    /// Paths to unlock. Upstream's CLI accepts a single path; ours
    /// accepts multiple (additive extension). Mutually exclusive
    /// with `--id`.
    pub paths: Vec<String>,

    /// Specify the Git LFS server to use. Ignored if the `lfs.url`
    /// config key is set.
    #[arg(short, long)]
    pub remote: Option<String>,

    /// Tell the server to remove the lock, even if it's owned by
    /// another user.
    #[arg(short, long)]
    pub force: bool,

    /// Specify a lock by its ID instead of path. Mutually exclusive
    /// with the positional paths.
    #[arg(short, long)]
    pub id: Option<String>,

    /// Write lock info as JSON to standard output if the command
    /// exits successfully.
    ///
    /// Intended for interoperation with external tools. If the command
    /// returns with a non-zero exit code, plain text messages are sent
    /// to standard error.
    #[arg(short, long)]
    pub json: bool,

    /// Refspec to send with the unlock request (extension over
    /// upstream).
    ///
    /// Defaults to the current branch's tracked upstream — same
    /// auto-resolution as `git lfs lock`.
    #[arg(long = "ref")]
    pub refspec: Option<String>,
}

/// Show information about Git LFS files in the index and working tree
///
/// Display paths of Git LFS files that are found in the tree at the
/// given reference. If no reference is given, scan the currently
/// checked-out branch.
///
/// An asterisk (`*`) after the OID indicates a full object, a minus
/// (`-`) indicates an LFS pointer.
///
/// Note: upstream's `--include` / `--exclude` path filters and the
/// `--deleted` flag (which shows the full history of the given
/// reference, including objects that have been deleted) aren't yet
/// supported. The two-references form (`git lfs ls-files <a> <b>`,
/// to show files modified between two refs) is also not yet
/// supported.
#[derive(Args)]
pub struct LsFilesArgs {
    /// Ref to list. Defaults to HEAD.
    pub refspec: Option<String>,

    /// Show the entire 64-character OID, instead of just the first
    /// 10.
    #[arg(short, long)]
    pub long: bool,

    /// Show the size of the LFS object in parentheses at the end of
    /// each line.
    #[arg(short, long)]
    pub size: bool,

    /// Show only the LFS-tracked file names.
    #[arg(short, long)]
    pub name_only: bool,

    /// Inspect the full history of the repository, not the current
    /// HEAD (or other provided reference).
    ///
    /// Includes previous versions of LFS objects that are no longer
    /// found in the current tree.
    #[arg(short, long)]
    pub all: bool,

    /// Show as much information as possible about an LFS file.
    ///
    /// Intended for manual inspection; the exact format may change
    /// at any time.
    #[arg(short, long)]
    pub debug: bool,

    /// Write Git LFS file information as JSON to standard output if
    /// the command exits successfully.
    ///
    /// Intended for interoperation with external tools. If `--debug`
    /// is also provided, that option takes precedence. If any of
    /// `--long`, `--size`, or `--name-only` are provided, those
    /// options will have no effect.
    #[arg(short, long)]
    pub json: bool,
}
