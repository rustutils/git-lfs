//! Clap CLI surface (struct `Cli` + subcommands).
//!
//! Extracted from `main.rs` so xtask (and any future tool) can
//! reuse the command tree for man-page generation, completion
//! scripts, etc. Keep this file focused on the clap derive — all
//! dispatch / business logic stays in main.rs and the per-command
//! modules.

use std::path::PathBuf;

use clap::{Parser, Subcommand};

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

#[derive(Subcommand)]
pub enum MigrateCmd {
    /// Rewrite history so files matching the include filter become LFS
    /// pointers. With `--no-rewrite`, history is preserved and one
    /// new commit is appended on top of HEAD with the named paths
    /// converted in place.
    Import {
        /// Without `--no-rewrite`: branches/refs to rewrite (empty =
        /// current branch). With `--no-rewrite`: working-tree paths
        /// to convert.
        args: Vec<String>,
        /// Walk every local branch and tag.
        #[arg(long)]
        everything: bool,
        /// Convert paths matching this glob (repeatable). Required
        /// unless `--above` is set or `--no-rewrite` is given.
        #[arg(short = 'I', long = "include")]
        include: Vec<String>,
        /// Exclude paths matching this glob (repeatable).
        #[arg(short = 'X', long = "exclude")]
        exclude: Vec<String>,
        /// Only convert files at least this large (e.g. `1mb`,
        /// `500k`).
        #[arg(long, default_value = "")]
        above: String,
        /// Don't rewrite history. Read named paths from the working
        /// tree, convert in place, append one new commit on top of
        /// HEAD.
        #[arg(long)]
        no_rewrite: bool,
        /// Commit message for the `--no-rewrite` commit.
        #[arg(short, long)]
        message: Option<String>,
        /// Skip the prompt confirming history rewrite. Currently we
        /// never prompt, so this is accepted as a no-op for parity
        /// with upstream's CLI surface.
        #[arg(long)]
        yes: bool,
        /// Walk every commit and convert files that *should* be LFS
        /// pointers (per their commit's `.gitattributes`) but
        /// currently aren't. Mutually exclusive with `--include`,
        /// `--exclude`, `--no-rewrite`.
        #[arg(long)]
        fixup: bool,
    },
    /// Inverse of import: rewrite history so LFS pointers become the
    /// raw bytes they reference. Requires the LFS objects to already
    /// be in the local store — `git lfs fetch` first if not. Pointers
    /// whose objects are missing are left as-is.
    Export {
        /// Branches / refs to rewrite. Empty = current branch.
        branches: Vec<String>,
        /// Walk every local branch and tag.
        #[arg(long)]
        everything: bool,
        /// Convert pointers at paths matching this glob (repeatable).
        /// Required.
        #[arg(short = 'I', long = "include")]
        include: Vec<String>,
        /// Don't convert pointers at paths matching this glob.
        #[arg(short = 'X', long = "exclude")]
        exclude: Vec<String>,
        /// Restrict the rewrite to commits reachable from these refs.
        /// Repeatable.
        #[arg(long = "include-ref")]
        include_ref: Vec<String>,
        /// Exclude commits reachable from these refs. Repeatable.
        #[arg(long = "exclude-ref")]
        exclude_ref: Vec<String>,
        /// Don't fetch missing LFS objects from the remote before the
        /// rewrite — leave their pointers in place.
        #[arg(long)]
        skip_fetch: bool,
        /// Write a comma-separated `<old>,<new>` mapping of every
        /// rewritten commit OID to the named file. Useful as input to
        /// `git filter-repo` or other downstream tools.
        #[arg(long = "object-map")]
        object_map: Option<std::path::PathBuf>,
        /// Print a per-commit progress line as the rewrite walks
        /// history.
        #[arg(long)]
        verbose: bool,
        /// Remote to consult when fetching missing LFS objects (default
        /// `origin`).
        #[arg(long)]
        remote: Option<String>,
        /// Skip the prompt confirming history rewrite. Currently we
        /// never prompt, so this is accepted as a no-op for parity
        /// with upstream's CLI surface.
        #[arg(long)]
        yes: bool,
    },
    /// Walk history and report file extensions by total size.
    /// Read-only — no objects or history change.
    Info {
        /// Branches / refs to scan. Empty = current branch.
        branches: Vec<String>,
        /// Walk every local branch and tag.
        #[arg(long)]
        everything: bool,
        /// Only include paths matching this glob (repeatable).
        #[arg(short = 'I', long = "include")]
        include: Vec<String>,
        /// Exclude paths matching this glob (repeatable).
        #[arg(short = 'X', long = "exclude")]
        exclude: Vec<String>,
        /// Only count files at least this large (e.g. `1mb`, `500k`).
        #[arg(long, default_value = "")]
        above: String,
        /// Maximum extension rows to show.
        #[arg(long, default_value_t = 5)]
        top: usize,
        /// How to handle existing LFS pointer blobs:
        /// `follow` (default), `ignore`, or `no-follow`.
        #[arg(long, default_value = "follow")]
        pointers: String,
    },
}

#[derive(Subcommand)]
pub enum Command {
    /// Run the clean filter: read content on stdin, write a pointer on stdout.
    Clean {
        /// Working-tree path of the file being cleaned (currently unused).
        path: Option<PathBuf>,
    },
    /// Run the smudge filter: read a pointer on stdin, write content on stdout.
    Smudge {
        /// Working-tree path of the file being smudged (currently unused).
        path: Option<PathBuf>,
        /// Pass the pointer text through unchanged; equivalent to
        /// `GIT_LFS_SKIP_SMUDGE=1`. Wired up by `install --skip-smudge`.
        #[arg(long)]
        skip: bool,
    },
    /// Configure git to invoke git-lfs as the clean/smudge/process filter,
    /// and install the LFS git hooks.
    Install {
        /// Set config in the local repo only (default: --global).
        #[arg(short, long)]
        local: bool,
        /// Overwrite existing config and hooks.
        #[arg(short, long)]
        force: bool,
        /// Only set the filter config; don't install hooks.
        #[arg(long)]
        skip_repo: bool,
        /// Configure the smudge filter to pass pointer text through
        /// unchanged. Use with a follow-up `git lfs pull` to download
        /// content on demand.
        #[arg(long)]
        skip_smudge: bool,
    },
    /// Reverse of `install`: clear the `filter.lfs.*` config and remove
    /// the LFS git hooks. Hooks that don't match what we'd write are left
    /// untouched.
    Uninstall {
        /// Operate on the local repo only (default: --global).
        #[arg(short, long)]
        local: bool,
        /// Only unset config; don't touch hooks.
        #[arg(long)]
        skip_repo: bool,
    },
    /// Track a file pattern with git-lfs by adding it to .gitattributes.
    /// With no patterns, lists currently-tracked patterns.
    Track {
        /// File patterns to track (e.g. "*.jpg", "data/*.bin").
        patterns: Vec<String>,
        /// Mark the tracked pattern as `lockable` (`*.psd lockable`).
        #[arg(short = 'l', long)]
        lockable: bool,
        /// Re-track an existing pattern, removing its `lockable` flag.
        #[arg(long)]
        not_lockable: bool,
        /// Print what would happen without modifying `.gitattributes` or
        /// re-staging files.
        #[arg(long)]
        dry_run: bool,
        /// Extra logging: print "Found N files previously added to Git
        /// matching pattern" lines.
        #[arg(short, long)]
        verbose: bool,
        /// Listing mode only: emit JSON instead of the human-readable
        /// listing.
        #[arg(long)]
        json: bool,
        /// Listing mode only: suppress the "Listing excluded patterns"
        /// section.
        #[arg(long)]
        no_excluded: bool,
        /// Treat each pattern as a literal filename — escape glob
        /// metacharacters (`*`, `?`, `[`, `]`, backslash, space) so
        /// the entry in `.gitattributes` matches that exact name even
        /// when it contains shell-glob characters.
        #[arg(long)]
        filename: bool,
    },
    /// Stop tracking a file pattern with git-lfs by removing it from
    /// .gitattributes. The matching pointer files in history (and the
    /// objects in the local store) are left in place.
    Untrack {
        /// File patterns to untrack.
        patterns: Vec<String>,
    },
    /// Run the long-running filter-process protocol with git over stdin/stdout.
    /// This is what git invokes via filter.lfs.process and is the batched
    /// alternative to per-invocation `clean`/`smudge`.
    FilterProcess {
        /// Pass smudge requests' pointer text through unchanged;
        /// equivalent to `GIT_LFS_SKIP_SMUDGE=1`. Wired up by
        /// `install --skip-smudge`.
        #[arg(long)]
        skip: bool,
    },
    /// Download every LFS object reachable from the given refs (default: HEAD)
    /// that isn't already in the local store. Walks history, dedupes by OID.
    Fetch {
        /// First positional arg is treated as a remote name (if it
        /// resolves); subsequent args are refs.
        args: Vec<String>,
        /// List the objects that would be fetched without downloading
        /// them (one `fetch <oid> => <path>` line per object).
        #[arg(long)]
        dry_run: bool,
        /// JSON output. With `--dry-run`, queries the server's batch
        /// endpoint to populate `actions` URLs.
        #[arg(long)]
        json: bool,
        /// Walk every local ref under `refs/heads/*` + `refs/tags/*`.
        #[arg(long)]
        all: bool,
        /// Re-download objects we already have (e.g. recovery from a
        /// corrupt local store).
        #[arg(long)]
        refetch: bool,
        /// Read refs from stdin, one per line. Blank lines dropped.
        #[arg(long)]
        stdin: bool,
        /// Run `prune` after the fetch completes.
        #[arg(long)]
        prune: bool,
        /// Comma-separated globs; only matching paths are fetched.
        /// Falls back to `lfs.fetchinclude` when omitted.
        #[arg(short = 'I', long)]
        include: Vec<String>,
        /// Comma-separated globs; matching paths are skipped. Falls
        /// back to `lfs.fetchexclude` when omitted.
        #[arg(short = 'X', long)]
        exclude: Vec<String>,
    },
    /// `fetch` then re-run the smudge filter so the working tree contains
    /// real LFS file contents instead of pointer text. Requires
    /// `git lfs install` to have wired up the smudge filter.
    Pull {
        /// Refs to scan for LFS pointers. Defaults to `HEAD`.
        refs: Vec<String>,
        /// Comma-separated globs; only matching paths are pulled.
        /// Falls back to `lfs.fetchinclude` when omitted.
        #[arg(short = 'I', long)]
        include: Vec<String>,
        /// Comma-separated globs; matching paths are skipped. Falls
        /// back to `lfs.fetchexclude` when omitted.
        #[arg(short = 'X', long)]
        exclude: Vec<String>,
    },
    /// Upload every LFS object reachable from the given refs that the
    /// remote doesn't already have. The "doesn't have" set is approximated
    /// by `refs/remotes/<remote>/*`; the LFS server's batch API also
    /// dedupes server-side so missing exclusions don't waste bandwidth.
    Push {
        /// Name of the remote (e.g. "origin") whose tracking refs are
        /// excluded from the upload set.
        remote: String,
        /// Refs (or, with `--object-id`, raw OIDs) to push. With
        /// `--all`, restricts the all-refs walk to these; with
        /// `--stdin`, ignored (a warning is emitted).
        args: Vec<String>,
        /// List the objects that would be pushed without actually
        /// uploading them (one `push <oid> => <path>` line per object).
        #[arg(long)]
        dry_run: bool,
        /// Push every local ref under `refs/heads/*` and `refs/tags/*`
        /// (intersected with `args` if any are given).
        #[arg(long)]
        all: bool,
        /// Read refs (or OIDs, with `--object-id`) from stdin, one per
        /// line. Blank lines are skipped.
        #[arg(long)]
        stdin: bool,
        /// Treat positional args / stdin entries as raw LFS OIDs
        /// rather than git refs, and upload those objects directly
        /// from the local store.
        #[arg(long)]
        object_id: bool,
    },
    /// Deprecated. Wraps `git clone` so the working tree is populated
    /// with pointer text first, then runs `git lfs pull` to download
    /// LFS content in batch. Modern `git clone` parallelizes the
    /// smudge filter and is no slower; prefer it.
    Clone {
        /// `git clone` and LFS pass-through args. The repository URL
        /// is required; an optional target directory follows.
        #[arg(trailing_var_arg = true, allow_hyphen_values = true)]
        args: Vec<String>,
    },
    /// Git post-checkout hook entry point. Receives `<prev-sha>
    /// <post-sha> <flag>` (flag is "1" if HEAD moved). Currently a
    /// no-op stub — exists so installed hook scripts don't fail. Real
    /// behavior arrives with `track --lockable`.
    PostCheckout { args: Vec<String> },
    /// Git post-commit hook entry point. No arguments. Currently a
    /// no-op stub.
    PostCommit { args: Vec<String> },
    /// Git post-merge hook entry point. Receives `<squash-flag>`.
    /// Currently a no-op stub.
    PostMerge { args: Vec<String> },
    /// Git pre-push hook entry point — not typically invoked by hand.
    /// Reads `<local-ref> <local-sha> <remote-ref> <remote-sha>` lines
    /// from stdin and uploads the LFS objects newly reachable from each
    /// `<local-sha>`.
    PrePush {
        /// Name of the remote being pushed to.
        remote: String,
        /// URL of the remote (informational; we use `lfs.url` config).
        url: Option<String>,
        /// List the objects that would be pushed without actually
        /// uploading them.
        #[arg(long)]
        dry_run: bool,
    },
    /// Print the git-lfs version and exit.
    Version,
    /// Debug helper: build a pointer from a file, parse one from disk
    /// or stdin, or just check whether some bytes are a valid pointer.
    Pointer {
        /// Build a pointer from this file (read content, hash, encode).
        #[arg(short, long)]
        file: Option<PathBuf>,
        /// Parse and display this existing pointer file.
        #[arg(short, long)]
        pointer: Option<PathBuf>,
        /// Read a pointer from stdin (mutually exclusive with --pointer).
        #[arg(long)]
        stdin: bool,
        /// Validity check mode: exit 0 if input parses, 1 if not, 2 if
        /// `--strict` and not byte-canonical.
        #[arg(long)]
        check: bool,
        /// In `--check`, also reject non-canonical pointers.
        #[arg(long)]
        strict: bool,
        /// Explicitly disable strict mode (paired with `--strict`).
        #[arg(long)]
        no_strict: bool,
    },
    /// Show the LFS environment: version, endpoints, on-disk paths, and
    /// the three `filter.lfs.*` config values.
    Env,
    /// List the configured LFS pointer extensions (`lfs.extension.<name>.*`).
    /// Extensions chain external clean/smudge programs around each LFS
    /// object; this prints their resolved configuration in priority order.
    Ext,
    /// Analyze or rewrite history for LFS conversion. Phase 1 ships
    /// `info` only; `import` and `export` will land in subsequent phases.
    Migrate {
        #[command(subcommand)]
        cmd: MigrateCmd,
    },
    /// Replace pointer text in the working tree with actual LFS object
    /// content. With no args, materializes every LFS pointer in HEAD's
    /// tree. With paths (literal file names or trailing-slash directory
    /// prefixes), restricts to matching pointers.
    ///
    /// During a merge conflict, `--to <path> --ours/--theirs/--base
    /// <file>` writes the LFS content from one of the conflicted
    /// stages to `<path>` (creating intermediate directories) so the
    /// user can compare or salvage versions.
    Checkout {
        /// Paths to check out. Empty = everything in HEAD's tree.
        /// In conflict mode (`--to`), exactly one path is required.
        paths: Vec<String>,
        /// Conflict-mode: write the chosen stage's content to this
        /// path instead of into the working tree. Resolves relative
        /// to the current directory.
        #[arg(long, value_name = "PATH")]
        to: Option<String>,
        /// Conflict-mode: pull from stage 2 (HEAD's version). Mutually
        /// exclusive with `--theirs` and `--base`.
        #[arg(long)]
        ours: bool,
        /// Conflict-mode: pull from stage 3 (the merging-in version).
        #[arg(long)]
        theirs: bool,
        /// Conflict-mode: pull from stage 1 (the common ancestor).
        #[arg(long)]
        base: bool,
    },
    /// Delete local LFS objects that aren't reachable from HEAD or any
    /// unpushed commit. Reclaims disk for repos whose history has moved
    /// past their objects.
    Prune {
        /// Don't delete anything; just report what would go.
        #[arg(short, long)]
        dry_run: bool,
        /// Print each prunable object's OID and size.
        #[arg(short, long)]
        verbose: bool,
    },
    /// Check the integrity of LFS objects and pointers reachable from
    /// `<refspec>` (default: HEAD). Exit 1 if anything is corrupt.
    Fsck {
        /// Ref to scan. Defaults to HEAD.
        refspec: Option<String>,
        /// Only check objects (verify store contents match pointer OIDs).
        #[arg(long)]
        objects: bool,
        /// Only check pointers (flag non-canonical pointer encodings).
        #[arg(long)]
        pointers: bool,
        /// Report problems but don't move corrupt objects to `<lfs>/bad/`.
        #[arg(short, long)]
        dry_run: bool,
    },
    /// Show staged + unstaged changes, classifying each blob as LFS,
    /// Git, or working-tree File.
    Status {
        /// Stable one-line-per-change format for scripts.
        #[arg(short, long)]
        porcelain: bool,
        /// Stable JSON output for scripts; only LFS entries are reported.
        #[arg(short, long)]
        json: bool,
    },
    /// Acquire an exclusive server-side lock on one or more files.
    /// Other users will be unable to push changes to a locked file.
    Lock {
        /// Paths to lock (repo-relative or absolute, must resolve inside
        /// the working tree).
        paths: Vec<String>,
        /// Specify which remote to use when interacting with locks.
        #[arg(short, long)]
        remote: Option<String>,
        /// Refspec to associate the lock with. Defaults to the current
        /// branch's tracked upstream (`branch.<current>.merge`) or the
        /// current branch's full ref (`refs/heads/<branch>`).
        #[arg(long = "ref")]
        refspec: Option<String>,
        /// Stable JSON output for scripts.
        #[arg(short, long)]
        json: bool,
    },
    /// List file locks held on the server.
    Locks {
        /// Specify which remote to use when interacting with locks.
        #[arg(short, long)]
        remote: Option<String>,
        /// Filter results to a particular path.
        #[arg(short, long)]
        path: Option<String>,
        /// Filter results to a particular lock id.
        #[arg(short, long)]
        id: Option<String>,
        /// Maximum number of results to return.
        #[arg(short, long)]
        limit: Option<u32>,
        /// Refspec to filter locks by (defaults to current branch /
        /// tracked upstream — same auto-resolution as `git lfs lock`).
        #[arg(long = "ref")]
        refspec: Option<String>,
        /// Verify ownership: prefix locks owned by the authenticated user
        /// with `O ` (others get `  `).
        #[arg(long)]
        verify: bool,
        /// Stable JSON output for scripts.
        #[arg(short, long)]
        json: bool,
    },
    /// Release a file lock previously acquired with `git lfs lock`.
    /// Either provide one or more paths, or `--id <id>` (mutually
    /// exclusive).
    Unlock {
        /// Paths to unlock; mutually exclusive with `--id`.
        paths: Vec<String>,
        /// Lock id to release; mutually exclusive with paths.
        #[arg(short, long)]
        id: Option<String>,
        /// Forcibly break another user's lock(s).
        #[arg(short, long)]
        force: bool,
        /// Specify which remote to use when interacting with locks.
        #[arg(short, long)]
        remote: Option<String>,
        /// Refspec to send with the unlock request (defaults to current
        /// branch / tracked upstream).
        #[arg(long = "ref")]
        refspec: Option<String>,
        /// Stable JSON output for scripts.
        #[arg(short, long)]
        json: bool,
    },
    /// List LFS-tracked files visible at a ref (default: HEAD), or across
    /// all reachable history with `--all`.
    LsFiles {
        /// Ref to list. Defaults to HEAD.
        refspec: Option<String>,
        /// Show full 64-char OID instead of the 10-char prefix.
        #[arg(short, long)]
        long: bool,
        /// Append humanized size in parens.
        #[arg(short, long)]
        size: bool,
        /// Print only the path.
        #[arg(short, long)]
        name_only: bool,
        /// Walk every reachable ref's full history.
        #[arg(short, long)]
        all: bool,
        /// Multi-line per-file block (size, checkout, download, oid, version).
        #[arg(short, long)]
        debug: bool,
        /// Stable JSON output for scripts.
        #[arg(short, long)]
        json: bool,
    },
}
