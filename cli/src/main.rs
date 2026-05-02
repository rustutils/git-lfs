use std::io::{self, BufRead, BufWriter, Read, Write};
use std::process::ExitCode;

use clap::{CommandFactory, Parser};
use git_lfs_filter::{CleanExtension, SmudgeExtension, clean, filter_process, smudge_with_fetch};
use git_lfs_store::Store;

mod checkout;
mod clone;
mod env;
mod ext;
mod fetch;
mod fetcher;
mod fsck;
mod hooks;
mod http_client;
mod install;
mod lock;
mod lock_cache;
mod lockable;
mod locks_verify;
mod ls_files;
mod migrate;
mod pointer_cmd;
mod pre_push;
mod prune;
mod pull;
mod push;
mod status;
mod track;
mod track_cmd;
mod update;

use fetcher::LfsFetcher;

use git_lfs::args::{
    CheckoutArgs, CleanArgs, Cli, CloneArgs, Command, EnvArgs, ExtArgs, FetchArgs,
    FilterProcessArgs, FsckArgs, InstallArgs, LockArgs, LocksArgs, LsFilesArgs, MigrateArgs,
    MigrateCmd, MigrateExportArgs, MigrateImportArgs, MigrateInfoArgs, PointerArgs,
    PostCheckoutArgs, PostCommitArgs, PostMergeArgs, PrePushArgs, PruneArgs, PullArgs, PushArgs,
    SmudgeArgs, StatusArgs, TrackArgs, UninstallArgs, UnlockArgs, UntrackArgs, UpdateArgs,
    VersionArgs,
};

fn main() -> ExitCode {
    let cli = Cli::parse();
    if cli.version {
        println!("git-lfs/{} (rust)", env!("CARGO_PKG_VERSION"));
        return ExitCode::SUCCESS;
    }
    let Some(command) = cli.command else {
        // Mimic clap's default error path when no subcommand is given.
        Cli::command().print_help().ok();
        return ExitCode::FAILURE;
    };
    match dispatch(command) {
        Ok(code) => ExitCode::from(code),
        Err(e) => {
            eprintln!("git-lfs: {e}");
            ExitCode::FAILURE
        }
    }
}

/// Path-shaped git env vars that interpret relative values against
/// the caller's cwd. We canonicalize them once up front so subprocess
/// invocations using `git -C <dir>` don't re-resolve them.
const PATH_GIT_ENV_VARS: &[&str] = &[
    "GIT_DIR",
    "GIT_WORK_TREE",
    "GIT_COMMON_DIR",
    "GIT_INDEX_FILE",
    "GIT_OBJECT_DIRECTORY",
];

/// Snapshot of `PATH_GIT_ENV_VARS` taken before [`canonicalize_path_envs`]
/// rewrites them to absolute paths. `git lfs env` reports the original
/// (possibly relative) values to match upstream's output.
static ORIGINAL_PATH_ENVS: std::sync::OnceLock<Vec<(&'static str, std::ffi::OsString)>> =
    std::sync::OnceLock::new();

/// Look up the *original* value of a path-shaped git env var, before
/// canonicalization. Returns `None` if the variable wasn't set or
/// wasn't in the path-shaped allowlist.
pub fn original_path_env(name: &str) -> Option<std::ffi::OsString> {
    ORIGINAL_PATH_ENVS
        .get()?
        .iter()
        .find(|(k, _)| *k == name)
        .map(|(_, v)| v.clone())
}

/// Make any relative path-shaped git env var absolute against `base`,
/// so subsequent `git -C <some_dir>` calls don't re-resolve them
/// against the wrong directory. Operates in place via `set_var`. Saves
/// the originals via [`original_path_env`] so `git lfs env` can still
/// echo the user-visible value.
fn canonicalize_path_envs(base: &std::path::Path) {
    let mut snapshot = Vec::new();
    for name in PATH_GIT_ENV_VARS {
        let Some(raw) = std::env::var_os(name) else {
            continue;
        };
        snapshot.push((*name, raw.clone()));
        if raw.is_empty() {
            continue;
        }
        let p = std::path::Path::new(&raw);
        if p.is_absolute() {
            continue;
        }
        let absolute = base.join(p);
        // SAFETY: We're early in `dispatch` before any threads are
        // spawned. `set_var` is unsafe in 2024 edition because of
        // multi-threaded races; the single-threaded prelude here is
        // exactly the documented safe usage pattern.
        unsafe {
            std::env::set_var(name, absolute);
        }
    }
    let _ = ORIGINAL_PATH_ENVS.set(snapshot);
}

/// Reduce the per-flag scope toggles for `install` / `uninstall` to a
/// single [`install::InstallScope`]. Multiple scope flags is a usage
/// error matching upstream's exact wording.
fn resolve_install_scope(
    local: bool,
    system: bool,
    worktree: bool,
    file: Option<std::path::PathBuf>,
) -> Result<install::InstallScope, &'static str> {
    let count = [local, system, worktree, file.is_some()]
        .iter()
        .filter(|b| **b)
        .count();
    if count > 1 {
        return Err(
            "Only one of the --local, --system, --worktree, and --file options can be specified.",
        );
    }
    Ok(if let Some(p) = file {
        install::InstallScope::File(p)
    } else if local {
        install::InstallScope::Local
    } else if system {
        install::InstallScope::System
    } else if worktree {
        install::InstallScope::Worktree
    } else {
        install::InstallScope::Global
    })
}

/// If `e` is a `git config <scope> ...` failure, render it on stdout
/// in upstream's `error running 'git config ...': <stderr>` shape and
/// return exit code 2. Otherwise return `None` and let the caller
/// propagate the error normally. The `>out.log` redirection in
/// t-install test 9 and t-install-worktree test 4 means this has to
/// land on stdout, not stderr.
fn handle_install_config_failure(e: &install::InstallError) -> Option<u8> {
    if let install::InstallError::ConfigCommandFailed { .. } = e {
        println!("{e}");
        Some(2)
    } else {
        None
    }
}

/// Print upstream's `Not in a Git repository.` to stdout and return a
/// non-zero exit code if `cwd` isn't inside any git repo. Returns
/// `None` to keep going. Mirrors the error path test 7 of t-uninstall
/// (and the analogous t-install test) asserts on.
fn bail_if_outside_repo(cwd: &std::path::Path) -> Option<u8> {
    if git_lfs_git::git_dir(cwd).is_err() {
        println!("Not in a Git repository.");
        return Some(128);
    }
    None
}

/// `GIT_LFS_SKIP_SMUDGE=1` (any value other than empty/0/false) tells
/// the smudge filter to leave pointer text in place rather than fetch.
/// Used by clones that intentionally don't materialize content (e.g.
/// CI partial clones, t-pull's "skip" tests).
fn skip_smudge_env() -> bool {
    match std::env::var_os("GIT_LFS_SKIP_SMUDGE") {
        None => false,
        Some(v) => {
            let s = v.to_string_lossy();
            !matches!(s.as_ref(), "" | "0" | "false" | "False" | "FALSE")
        }
    }
}

/// Print a migrate error and choose an exit code. Usage-shaped errors
/// (`MigrateError::Usage`) print verbatim and exit with code 2 —
/// upstream's `if PIPESTATUS == 1 fatal` checks distinguish usage
/// errors from general failures, and exact-match tests like
/// `[$(cmd) = "Cannot use ..."]` don't care about the code anyway.
fn handle_migrate_error(e: migrate::MigrateError) -> u8 {
    match e {
        migrate::MigrateError::Usage(msg) => {
            eprintln!("{msg}");
            2
        }
        other => {
            eprintln!("git-lfs: {other}");
            1
        }
    }
}

/// Split each entry in `values` on commas and trim whitespace, dropping
/// empties. Mirrors upstream's `--include="*.md, *.txt"` parsing — clap
/// gives us one Vec entry per `--include` flag, and a comma-separated
/// list within the entry needs to expand. Repeated flags (e.g.
/// `--include foo --include bar`) are also flattened.
fn split_csv(values: &[String]) -> Vec<String> {
    values
        .iter()
        .flat_map(|s| s.split(','))
        .map(str::trim)
        .filter(|s| !s.is_empty())
        .map(str::to_owned)
        .collect()
}

/// Read configured pointer extensions and convert to the filter crate's
/// runtime form. Skips entries whose `clean` is empty or whose priority
/// is outside the spec's 0-9 range — those wouldn't produce a valid
/// `ext-N-<name>` line anyway.
fn collect_clean_extensions(cwd: &std::path::Path) -> Vec<CleanExtension> {
    git_lfs_git::list_extensions(cwd)
        .into_iter()
        .filter_map(|ext| {
            if ext.clean.trim().is_empty() {
                return None;
            }
            let priority = u8::try_from(ext.priority).ok().filter(|&p| p <= 9)?;
            Some(CleanExtension {
                name: ext.name,
                priority,
                command: ext.clean,
            })
        })
        .collect()
}

/// Same shape as [`collect_clean_extensions`] but builds the `smudge`
/// side of the registered extensions. Used by the smudge filter,
/// filter-process, and the pull/checkout materialize loops to invert
/// what `clean` did during commit.
fn collect_smudge_extensions(cwd: &std::path::Path) -> Vec<SmudgeExtension> {
    git_lfs_git::list_extensions(cwd)
        .into_iter()
        .filter_map(|ext| {
            if ext.smudge.trim().is_empty() {
                return None;
            }
            let priority = u8::try_from(ext.priority).ok().filter(|&p| p <= 9)?;
            Some(SmudgeExtension {
                name: ext.name,
                priority,
                command: ext.smudge,
            })
        })
        .collect()
}

fn dispatch(cmd: Command) -> Result<u8, Box<dyn std::error::Error>> {
    let cwd = std::env::current_dir()?;
    // `GIT_DIR` / `GIT_WORK_TREE` (and the auxiliary `GIT_OBJECT_*`
    // variants) come in relative to the *caller's* cwd. Many of our
    // subprocess invocations later use `git -C <repo_root>`, which
    // would re-resolve those relative paths against the wrong base.
    // Canonicalize once up front so every downstream `git` call sees
    // absolute paths regardless of where it's chdir'd to.
    canonicalize_path_envs(&cwd);

    // Sweep stale temp-object files (`<oid>-<random>` under
    // `<lfs>/tmp/objects/`) whose object is already complete in the
    // store. Mirrors upstream's `lfs.cleanupTempFiles` startup task.
    // Skipped silently when we're not in a repo (e.g. `git lfs
    // version` outside any repo).
    if let Ok(lfs_root) = git_lfs_git::lfs_dir(&cwd) {
        Store::new(lfs_root).cleanup_tmp_objects();
    }

    match cmd {
        Command::Clean(CleanArgs { path }) => {
            let _ = install::try_install_hooks(&cwd);
            let store = Store::new(git_lfs_git::lfs_dir(&cwd)?);
            // No `with_references` here: clean writes new content
            // computed from working-tree input, so alternate stores
            // can't satisfy the lookup (and we don't want to reuse
            // their inode for a freshly-staged file).
            let stdin = io::stdin().lock();
            let mut input: Box<dyn Read> = Box::new(stdin);
            let mut output: Box<dyn Write> = Box::new(BufWriter::new(io::stdout().lock()));
            let extensions = collect_clean_extensions(&cwd);
            let path_str = path
                .as_deref()
                .map(|p| p.to_string_lossy().into_owned())
                .unwrap_or_default();
            clean(&store, &mut input, &mut output, &path_str, &extensions)?;
            output.flush()?;
        }
        Command::Smudge(SmudgeArgs { path, skip }) => {
            let _ = install::try_install_hooks(&cwd);
            let store = Store::new(git_lfs_git::lfs_dir(&cwd)?)
                .with_references(git_lfs_git::lfs_alternate_dirs(&cwd).unwrap_or_default());
            let stdin = io::stdin().lock();
            let mut input: Box<dyn Read> = Box::new(stdin);
            let mut output: Box<dyn Write> = Box::new(BufWriter::new(io::stdout().lock()));
            if skip || skip_smudge_env() {
                io::copy(&mut input, &mut output)?;
            } else {
                let fetcher = LfsFetcher::from_repo(&cwd, &store)?;
                let smudge_extensions = collect_smudge_extensions(&cwd);
                let path_str = path
                    .as_deref()
                    .map(|p| p.to_string_lossy().into_owned())
                    .unwrap_or_default();
                smudge_with_fetch(
                    &store,
                    &mut input,
                    &mut output,
                    &path_str,
                    &smudge_extensions,
                    |p| fetcher.fetch(p),
                )?;
                fetcher.persist_access_mode();
            }
            output.flush()?;
        }
        Command::Install(InstallArgs {
            local,
            system,
            worktree,
            file,
            force,
            skip_repo,
            skip_smudge,
        }) => {
            let scope = match resolve_install_scope(local, system, worktree, file) {
                Ok(s) => s,
                Err(msg) => {
                    eprintln!("{msg}");
                    return Ok(2);
                }
            };
            if scope.is_repo_scope()
                && let Some(code) = bail_if_outside_repo(&cwd)
            {
                return Ok(code);
            }
            let opts = install::InstallOptions {
                scope,
                force,
                skip_repo,
                skip_smudge,
            };
            // Compute up-front whether hooks would be installed so the
            // success path can print "Updated Git hooks." in the same
            // order upstream does. Mirrors the `install_hooks_too`
            // condition inside `install::install`.
            let hooks_active = !opts.skip_repo
                && (opts.scope.is_repo_scope() || git_lfs_git::git_dir(&cwd).is_ok());
            match install::install(&cwd, &opts) {
                Ok(()) => {
                    if hooks_active {
                        println!("Updated Git hooks.");
                    }
                    println!("Git LFS initialized.");
                }
                Err(e @ install::InstallError::FilterAttribute { .. }) => {
                    // Mirrors upstream's `commands/command_install.go`:
                    // print `warning: <err>` + the `--force` hint on
                    // stdout, exit 2. t-install test 2 captures via
                    // `tee install.log` (stdout only) and greps for
                    // `(clean|smudge)" attribute should be`.
                    println!("warning: {e}");
                    println!("Run `git lfs install --force` to reset Git configuration.");
                    return Ok(2);
                }
                Err(install::InstallError::HookConflict { hook, existing }) => {
                    install::print_hook_conflict(&hook, &existing);
                    return Ok(2);
                }
                Err(e) if let Some(code) = handle_install_config_failure(&e) => {
                    return Ok(code);
                }
                Err(e) => return Err(Box::new(e)),
            }
        }
        Command::Uninstall(UninstallArgs {
            mode,
            local,
            system,
            worktree,
            file,
            skip_repo,
        }) => {
            let scope = match resolve_install_scope(local, system, worktree, file) {
                Ok(s) => s,
                Err(msg) => {
                    eprintln!("{msg}");
                    return Ok(2);
                }
            };
            let hooks_only = match mode.as_deref() {
                None => false,
                Some("hooks") => true,
                Some(other) => {
                    eprintln!("git-lfs: unknown mode {other:?}");
                    return Ok(2);
                }
            };
            if scope.is_repo_scope()
                && let Some(code) = bail_if_outside_repo(&cwd)
            {
                return Ok(code);
            }
            let opts = install::UninstallOptions {
                scope: scope.clone(),
                skip_repo,
                hooks_only,
            };
            if let Err(e) = install::uninstall(&cwd, &opts) {
                if matches!(e, install::InstallError::ConfigCommandFailed { .. }) {
                    // Uninstall is idempotent — a failing
                    // `git config <scope> --unset` (e.g. --worktree
                    // against a repo without the worktreeConfig
                    // extension) becomes a stdout warning and we keep
                    // going. Exit 0 matches upstream's t-uninstall-
                    // worktree test 4.
                    println!("{e}");
                    return Ok(0);
                }
                return Err(Box::new(e));
            }
            if hooks_only {
                // No "configuration removed" message: hooks-only mode
                // leaves the filter config exactly as-is.
            } else if scope.announces_global() {
                println!("Global Git LFS configuration has been removed.");
            } else {
                println!("Local Git LFS configuration has been removed.");
            }
        }
        Command::Clone(CloneArgs { args }) => {
            clone::run(&cwd, &args)?;
        }
        Command::FilterProcess(FilterProcessArgs { skip }) => {
            let _ = install::try_install_hooks(&cwd);
            let store = Store::new(git_lfs_git::lfs_dir(&cwd)?)
                .with_references(git_lfs_git::lfs_alternate_dirs(&cwd).unwrap_or_default());
            let fetcher = LfsFetcher::from_repo(&cwd, &store)?;
            let stdin = io::stdin().lock();
            let stdout = io::stdout().lock();
            let clean_extensions = collect_clean_extensions(&cwd);
            let smudge_extensions = collect_smudge_extensions(&cwd);
            // Honor `lfs.fetchinclude`/`lfs.fetchexclude`: paths
            // outside the include set or inside the exclude set get
            // pointer-text passthrough at smudge time, matching
            // upstream's filter-process behavior (test 2 of
            // t-filter-process). Patterns are pulled via
            // `build_pattern_set`, the same helper fetch/pull use.
            let include_set = fetch::build_pattern_set(&cwd, &[], "lfs.fetchinclude")?;
            let exclude_set = fetch::build_pattern_set(&cwd, &[], "lfs.fetchexclude")?;
            let path_filter = move |path: &str| -> bool {
                let p = std::path::Path::new(path);
                fetch::path_passes_filter(Some(p), &include_set, &exclude_set)
            };
            filter_process(
                &store,
                stdin,
                stdout,
                |p| fetcher.fetch(p),
                skip || skip_smudge_env(),
                &clean_extensions,
                &smudge_extensions,
                &path_filter,
            )?;
            fetcher.persist_access_mode();
        }
        Command::Fetch(FetchArgs {
            args,
            dry_run,
            json,
            all,
            refetch,
            stdin,
            prune,
            include,
            exclude,
        }) => {
            let stdin_lines: Vec<String> = if stdin {
                io::stdin()
                    .lock()
                    .lines()
                    .map_while(Result::ok)
                    .map(|l| l.trim().to_owned())
                    .filter(|l| !l.is_empty())
                    .collect()
            } else {
                Vec::new()
            };
            let opts = fetch::FetchOptions {
                args: &args,
                stdin_lines: &stdin_lines,
                dry_run,
                json,
                all,
                refetch,
                stdin,
                prune,
                include: &include,
                exclude: &exclude,
            };
            match fetch::fetch(&cwd, &opts) {
                Ok(outcome) => {
                    if !outcome.report.failed.is_empty() {
                        return Err("one or more objects failed to download".into());
                    }
                }
                Err(fetch::FetchCommandError::Usage(msg)) if msg == "Not in a Git repository." => {
                    // Test `t-fetch.sh::fetch: outside git repository`
                    // greps for this on stdout (`2>&1 > fetch.log`
                    // captures stdout only). Match upstream and emit
                    // here, then exit 128.
                    println!("{msg}");
                    return Ok(128);
                }
                Err(e) => return Err(e.into()),
            }
        }
        Command::Pull(PullArgs {
            refs,
            include,
            exclude,
        }) => {
            match pull::pull_with_filter(&cwd, &refs, &include, &exclude) {
                Ok(()) => {}
                Err(pull::PullCommandError::Fetch(fetch::FetchCommandError::Usage(msg)))
                    if msg == "Not in a Git repository." =>
                {
                    // Mirrors fetch's outside-repo handling for parity
                    // with `git lfs fetch` (and t-pull's `outside git
                    // repository` test, which expects exit 128).
                    println!("{msg}");
                    return Ok(128);
                }
                Err(e @ pull::PullCommandError::FetchFailures(_)) => {
                    // Per-object transfer failures (per-object-batch
                    // 404s, action-URL 4xx/5xx) — upstream exits 2
                    // here, and t-pull's `pull with invalid insteadof`
                    // greps for that exit code specifically.
                    eprintln!("git-lfs: {e}");
                    return Ok(2);
                }
                Err(e) => return Err(e.into()),
            }
        }
        Command::Push(PushArgs {
            remote,
            args,
            dry_run,
            all,
            stdin,
            object_id,
        }) => {
            let stdin_lines: Vec<String> = if stdin {
                io::stdin()
                    .lock()
                    .lines()
                    .map_while(Result::ok)
                    .map(|l| l.trim().to_owned())
                    .filter(|l| !l.is_empty())
                    .collect()
            } else {
                Vec::new()
            };
            let opts = push::PushOptions {
                args: &args,
                stdin_lines: &stdin_lines,
                dry_run,
                all,
                stdin,
                object_id,
            };
            let outcome = push::push(&cwd, &remote, &opts)?;
            if outcome.aborted {
                return Ok(2);
            }
            if !outcome.report.failed.is_empty() {
                return Err("one or more objects failed to upload".into());
            }
        }
        Command::PostCheckout(PostCheckoutArgs { args }) => {
            hooks::post_checkout(&cwd, &args)?;
        }
        Command::PostCommit(PostCommitArgs { args }) => {
            hooks::post_commit(&cwd, &args)?;
        }
        Command::PostMerge(PostMergeArgs { args }) => {
            hooks::post_merge(&cwd, &args)?;
        }
        Command::PrePush(PrePushArgs {
            remote,
            url: _,
            dry_run,
        }) => {
            let stdin = io::stdin().lock();
            let outcome = pre_push::pre_push(&cwd, &remote, stdin, dry_run)?;
            if outcome.aborted {
                return Ok(2);
            }
            if !outcome.report.failed.is_empty() {
                return Err("pre-push: one or more objects failed to upload".into());
            }
        }
        Command::Track(TrackArgs {
            patterns,
            lockable,
            not_lockable,
            dry_run,
            verbose,
            json,
            no_excluded,
            filename,
            no_modify_attrs,
        }) => {
            return track_cmd::run(track_cmd::Args {
                cwd: &cwd,
                patterns: &patterns,
                lockable,
                not_lockable,
                dry_run,
                verbose,
                json,
                no_excluded,
                filename,
                no_modify_attrs,
            });
        }
        Command::Version(VersionArgs) => {
            println!("git-lfs/{} (rust)", env!("CARGO_PKG_VERSION"));
        }
        Command::Pointer(PointerArgs {
            file,
            pointer,
            stdin,
            check,
            strict,
            no_strict,
        }) => {
            let opts = pointer_cmd::Options {
                file,
                pointer,
                stdin,
                check,
                strict,
                no_strict,
            };
            // Pointer's exit codes are semantic: 1 = mismatch / parse
            // failure, 2 = `--strict` non-canonical. Propagate verbatim.
            let code = pointer_cmd::run(&opts)?;
            return Ok(code as u8);
        }
        Command::Env(EnvArgs) => {
            env::run(&cwd)?;
        }
        Command::Ext(ExtArgs) => {
            ext::run(&cwd)?;
        }
        Command::Update(UpdateArgs { force, manual }) => match update::run(&cwd, force, manual) {
            Ok(code) => return Ok(code),
            Err(update::UpdateError::NotInRepo) => {
                // Stdout, not stderr: upstream prints to stdout here
                // and `t-update` test 4 redirects only `>check.log`
                // (its `2>&1` is in the wrong order to capture stderr).
                println!("Not in a Git repository.");
                return Ok(128);
            }
            Err(e) => return Err(Box::new(e)),
        },
        Command::Migrate(MigrateArgs { cmd }) => match cmd {
            MigrateCmd::Export(MigrateExportArgs {
                branches,
                everything,
                include,
                exclude,
                include_ref,
                exclude_ref,
                skip_fetch,
                object_map,
                verbose,
                remote,
                yes: _,
            }) => {
                let opts = migrate::ExportOptions {
                    branches,
                    everything,
                    include: split_csv(&include),
                    exclude: split_csv(&exclude),
                    include_ref,
                    exclude_ref,
                    skip_fetch,
                    object_map,
                    verbose,
                    remote,
                };
                if let Err(e) = migrate::export(&cwd, &opts) {
                    return Ok(handle_migrate_error(e));
                }
            }
            MigrateCmd::Import(MigrateImportArgs {
                args,
                everything,
                include,
                exclude,
                include_ref,
                exclude_ref,
                above,
                no_rewrite,
                message,
                yes,
                fixup,
                skip_fetch,
                object_map,
                verbose,
                remote,
            }) => {
                let above_bytes = migrate::parse_size(&above)?;
                let (branches, paths) = if no_rewrite {
                    (Vec::new(), args)
                } else {
                    (args, Vec::new())
                };
                let opts = migrate::ImportOptions {
                    branches,
                    everything,
                    include: split_csv(&include),
                    exclude: split_csv(&exclude),
                    include_ref,
                    exclude_ref,
                    above: above_bytes,
                    no_rewrite,
                    message,
                    paths,
                    fixup,
                    skip_fetch,
                    object_map,
                    verbose,
                    remote,
                    yes,
                };
                let _ = install::try_install_hooks(&cwd);
                if let Err(e) = migrate::import(&cwd, &opts) {
                    return Ok(handle_migrate_error(e));
                }
            }
            MigrateCmd::Info(MigrateInfoArgs {
                branches,
                everything,
                include,
                exclude,
                include_ref,
                exclude_ref,
                above,
                top,
                pointers,
                unit,
                skip_fetch: _,
                remote: _,
                fixup,
            }) => {
                let pointer_mode = match pointers.as_deref() {
                    Some("follow") => migrate::PointerMode::Follow,
                    Some("no-follow") => migrate::PointerMode::NoFollow,
                    Some("ignore") => migrate::PointerMode::Ignore,
                    Some(other) => {
                        return Ok(handle_migrate_error(migrate::MigrateError::Usage(format!(
                            "Unsupported --pointers option value: {other:?}"
                        ))));
                    }
                    // No `--pointers` flag: `--fixup` implies `ignore`
                    // (we want to see what *should* be LFS but isn't);
                    // otherwise default to `follow`.
                    None if fixup => migrate::PointerMode::Ignore,
                    None => migrate::PointerMode::Follow,
                };
                let above_bytes = migrate::parse_size(&above)?;
                let unit_bytes = match unit.as_deref() {
                    None | Some("") => None,
                    Some(s) => Some(migrate::parse_size(s)?),
                };
                let opts = migrate::InfoOptions {
                    branches,
                    everything,
                    include: split_csv(&include),
                    exclude: split_csv(&exclude),
                    include_ref,
                    exclude_ref,
                    above: above_bytes,
                    top,
                    pointers: pointer_mode,
                    unit: unit_bytes,
                    fixup,
                };
                if let Err(e) = migrate::info(&cwd, &opts) {
                    return Ok(handle_migrate_error(e));
                }
            }
        },
        Command::Checkout(CheckoutArgs {
            paths,
            to,
            ours,
            theirs,
            base,
        }) => {
            let opts = checkout::Options {
                paths,
                to,
                ours,
                theirs,
                base,
            };
            match checkout::run(&cwd, &opts) {
                Ok(()) => {}
                Err(checkout::CheckoutError::NotInWorkTree) => {
                    // Bare repo: matches t-status / t-checkout
                    // bare-repo expectation. Exit 0 with the
                    // upstream-compatible message.
                    println!("This operation must be run in a work tree.");
                }
                Err(checkout::CheckoutError::NotInRepo) => {
                    // Outside any repo. Exit 128 with the wording the
                    // t-checkout outside-repo test greps for.
                    println!("Not in a Git repository.");
                    return Ok(128);
                }
                Err(checkout::CheckoutError::Usage(msg)) => {
                    // Conflict-mode flag validation errors. Upstream
                    // exits via `Exit()` which writes to stderr and
                    // returns 2; mirror that.
                    eprintln!("{msg}");
                    return Ok(2);
                }
                Err(e) => return Err(e.into()),
            }
        }
        Command::Prune(PruneArgs { dry_run, verbose }) => {
            let opts = prune::Options { dry_run, verbose };
            prune::run(&cwd, &opts)?;
        }
        Command::Fsck(FsckArgs {
            refspec,
            objects,
            pointers,
            dry_run,
        }) => {
            let _ = install::try_install_hooks(&cwd);
            let mode = match (objects, pointers) {
                (true, false) => fsck::Mode::Objects,
                (false, true) => fsck::Mode::Pointers,
                _ => fsck::Mode::Both,
            };
            let opts = fsck::Options { mode, dry_run };
            let code = fsck::run(&cwd, refspec.as_deref(), &opts)?;
            return Ok(code as u8);
        }
        Command::Status(StatusArgs { porcelain, json }) => {
            let format = if json {
                status::Format::Json
            } else if porcelain {
                status::Format::Porcelain
            } else {
                status::Format::Default
            };
            match status::run(&cwd, format) {
                Ok(()) => {}
                Err(status::StatusError::NotInRepo) => {
                    // Match `git lfs fetch` / `pull`: emit the message
                    // on stdout and exit 128 so the t-status outside-
                    // repo test (and parity with upstream) holds.
                    println!("Not in a Git repository.");
                    return Ok(128);
                }
                Err(status::StatusError::NotInWorkTree) => {
                    // Bare repo: status has nothing to compare a work
                    // tree against. Upstream exits non-zero here.
                    println!("This operation must be run in a work tree.");
                    return Ok(1);
                }
                Err(e) => return Err(e.into()),
            }
        }
        Command::Lock(LockArgs {
            paths,
            remote,
            refspec,
            json,
        }) => {
            let opts = lock::LockOptions {
                remote,
                refspec,
                json,
            };
            let ok = lock::lock(&cwd, &paths, &opts)?;
            if !ok {
                return Err("one or more locks failed".into());
            }
        }
        Command::Locks(LocksArgs {
            remote,
            path,
            id,
            limit,
            refspec,
            verify,
            local,
            json,
        }) => {
            let opts = lock::LocksOptions {
                remote,
                refspec,
                path,
                id,
                limit,
                verify,
                local,
                json,
            };
            lock::locks(&cwd, &opts)?;
        }
        Command::Unlock(UnlockArgs {
            paths,
            id,
            force,
            remote,
            refspec,
            json,
        }) => {
            let opts = lock::UnlockOptions {
                remote,
                refspec,
                id,
                force,
                json,
            };
            let ok = lock::unlock(&cwd, &paths, &opts)?;
            if !ok {
                return Err("one or more unlocks failed".into());
            }
        }
        Command::LsFiles(LsFilesArgs {
            refspec,
            long,
            size,
            name_only,
            all,
            debug,
            json,
        }) => {
            let format = if json {
                ls_files::Format::Json
            } else if debug {
                ls_files::Format::Debug
            } else {
                ls_files::Format::Default
            };
            let opts = ls_files::Options {
                long,
                show_size: size,
                name_only,
                all,
                format,
            };
            ls_files::run(&cwd, refspec.as_deref(), &opts)?;
        }
        Command::Untrack(UntrackArgs { patterns }) => {
            if patterns.is_empty() {
                return Err("git lfs untrack <pattern> [pattern...]".into());
            }
            // Resolve the work-tree root: a bare repo or being outside
            // any repo entirely is a "not in a git repository" error
            // matching git's exit code (128). When cwd is inside the
            // work tree we prefer it (so `cd a; git lfs untrack foo`
            // edits `a/.gitattributes`), falling back to the work-tree
            // root when GIT_WORK_TREE points outside cwd.
            let work_tree = match git_lfs_git::work_tree_root(&cwd) {
                Ok(p) => p,
                Err(_) => {
                    eprintln!("fatal: not in a git repository");
                    return Ok(128);
                }
            };
            let attrs_dir = if cwd.starts_with(&work_tree) {
                cwd.clone()
            } else {
                work_tree
            };
            let _ = install::try_install_hooks(&cwd);
            let outcome = track::untrack(&attrs_dir, &patterns)?;
            for p in &outcome.removed {
                println!("Untracking \"{p}\"");
            }
            for p in &outcome.missing {
                println!("\"{p}\" was not tracked");
            }
        }
    }
    Ok(0)
}
