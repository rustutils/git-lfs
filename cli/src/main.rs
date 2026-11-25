use std::io::{self, BufRead, BufWriter, Read, Write};
use std::process::ExitCode;

use clap::{Arg, CommandFactory, FromArgMatches};
use git_lfs_filter::{
    CleanExtension, SmudgeError, SmudgeExtension, clean, filter_process, smudge_with_fetch,
};
use git_lfs_store::Store;

mod action_auth;
mod checkout;
mod client_key;
mod clone;
mod completion;
mod dedup;
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
mod lock_client;
mod lockable;
mod locks_verify;
mod logs;
mod ls_files;
mod merge_driver;
mod migrate;
mod pointer_cmd;
mod pre_push;
mod prune;
mod pull;
mod push;
mod smudge_fetcher;
mod standalone_file;
mod status;
mod track;
mod track_cmd;
mod update;

use fetcher::LfsFetcher;

use git_lfs::args::{
    CheckoutArgs, CleanArgs, Cli, CloneArgs, Command, CompletionArgs, DedupArgs, EnvArgs, ExtArgs,
    ExtCmd, ExtListArgs, FetchArgs, FilterProcessArgs, FsckArgs, InstallArgs, LockArgs, LocksArgs,
    LogsArgs, LogsSub, LsFilesArgs, MergeDriverArgs, MigrateArgs, MigrateCmd, MigrateExportArgs,
    MigrateImportArgs, MigrateInfoArgs, PointerArgs, PostCheckoutArgs, PostCommitArgs,
    PostMergeArgs, PrePushArgs, PruneArgs, PullArgs, PushArgs, SmudgeArgs, StandaloneFileArgs,
    StatusArgs, TrackArgs, UninstallArgs, UnlockArgs, UntrackArgs, UpdateArgs, VersionArgs,
};

/// Strip the backticks from all help text in the clap command tree.
///
/// The help text keeps them for the man pages and the manual, which render
/// them as code. In a terminal they're noise.
fn strip_backticks_in_help(cmd: clap::Command) -> clap::Command {
    let mut cmd = cmd.mut_args(strip_backticks_in_arg);
    let about = cmd.get_about().map(|s| s.to_string().replace('`', ""));
    let long_about = cmd.get_long_about().map(|s| s.to_string().replace('`', ""));
    if let Some(s) = about {
        cmd = cmd.about(s);
    }
    if let Some(s) = long_about {
        cmd = cmd.long_about(s);
    }
    cmd.mut_subcommands(strip_backticks_in_help)
}

fn strip_backticks_in_arg(arg: Arg) -> Arg {
    let help = arg.get_help().map(|s| s.to_string().replace('`', ""));
    let long_help = arg.get_long_help().map(|s| s.to_string().replace('`', ""));
    let mut arg = arg;
    if let Some(s) = help {
        arg = arg.help(s);
    }
    if let Some(s) = long_help {
        arg = arg.long_help(s);
    }
    arg
}

fn main() -> ExitCode {
    let cmd = strip_backticks_in_help(Cli::command());
    // The completion scripts' request, handled before clap so that the
    // words being completed aren't parsed as our own arguments.
    let argv: Vec<String> = std::env::args().skip(1).collect();
    if let Some((first, rest)) = argv.split_first() {
        let descriptions = match first.as_str() {
            "__complete" => Some(true),
            "__completeNoDesc" => Some(false),
            _ => None,
        };
        if let Some(descriptions) = descriptions {
            return ExitCode::from(completion::run_complete(cmd, rest, descriptions));
        }
    }
    let matches = cmd.get_matches();
    let cli = Cli::from_arg_matches(&matches).unwrap_or_else(|e| e.exit());
    if cli.version {
        println!("git-lfs/{} (rust)", env!("CARGO_PKG_VERSION"));
        return ExitCode::SUCCESS;
    }
    let Some(command) = cli.command else {
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

/// Git environment variables holding paths, which Git resolves against the
/// current directory.
const PATH_GIT_ENV_VARS: &[&str] = &[
    "GIT_DIR",
    "GIT_WORK_TREE",
    "GIT_COMMON_DIR",
    "GIT_INDEX_FILE",
    "GIT_OBJECT_DIRECTORY",
];

/// The values of [`PATH_GIT_ENV_VARS`] before [`canonicalize_path_envs`]
/// made them absolute.
static ORIGINAL_PATH_ENVS: std::sync::OnceLock<Vec<(&'static str, std::ffi::OsString)>> =
    std::sync::OnceLock::new();

/// The value environment variable `name` had at startup, if it's one of
/// [`PATH_GIT_ENV_VARS`] and was set.
///
/// `git lfs env` prints these, as upstream does.
pub fn original_path_env(name: &str) -> Option<std::ffi::OsString> {
    ORIGINAL_PATH_ENVS
        .get()?
        .iter()
        .find(|(k, _)| *k == name)
        .map(|(_, v)| v.clone())
}

/// Make relative [`PATH_GIT_ENV_VARS`] absolute against `base`.
///
/// Otherwise a later `git -C <dir>` would resolve them against `<dir>`.
/// The original values stay available through [`original_path_env`].
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
        // SAFETY: This runs at the start of `dispatch`, before any other
        // thread exists, so nothing can read the environment at the same
        // time.
        unsafe {
            std::env::set_var(name, absolute);
        }
    }
    let _ = ORIGINAL_PATH_ENVS.set(snapshot);
}

/// Turn the scope flags of `install` and `uninstall` into a scope.
///
/// With no flag it's global. More than one flag is an error, with
/// upstream's message.
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

/// Print a failed `git config` write and return exit code 2, or return
/// `None` for any other error.
///
/// It goes to stdout, since t-install.sh and t-install-worktree.sh only
/// capture stdout.
fn handle_install_config_failure(e: &install::InstallError) -> Option<u8> {
    if let install::InstallError::ConfigCommandFailed { .. } = e {
        println!("{e}");
        Some(2)
    } else {
        None
    }
}

/// Outside a repository, print upstream's message to stdout and return
/// exit code 128, as upstream's `requireInRepo` does.
fn bail_if_outside_repo(cwd: &std::path::Path) -> Option<u8> {
    if git_lfs_git::git_dir(cwd).is_err() {
        println!("Not in a Git repository.");
        return Some(128);
    }
    None
}

/// Whether `GIT_LFS_SKIP_SMUDGE` is true, which makes smudge write
/// pointers instead of content.
fn skip_smudge_env() -> bool {
    git_lfs_git::config::env_bool("GIT_LFS_SKIP_SMUDGE", false)
}

/// Print a migrate error and return the exit code.
///
/// Usage errors print as they are, since tests compare them exactly, and
/// exit 2 like upstream's `Exit`. Other errors exit 1.
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

/// Split each of `values` on commas, trimming whitespace and dropping
/// empty entries.
///
/// Upstream accepts `--include="*.md, *.txt"`.
fn split_csv(values: &[String]) -> Vec<String> {
    values
        .iter()
        .flat_map(|s| s.split(','))
        .map(str::trim)
        .filter(|s| !s.is_empty())
        .map(str::to_owned)
        .collect()
}

/// `core.sharedRepository`, for [`Store::with_shared_repository`].
///
/// `None` if it's unset or can't be read, in which case the store uses
/// the umask.
pub fn shared_repo_config(cwd: &std::path::Path) -> Option<String> {
    git_lfs_git::config::get_effective(cwd, "core.sharedRepository")
        .ok()
        .flatten()
}

/// The configured pointer extensions that have a clean command.
///
/// Extensions with a priority outside 0 to 9 are skipped, since a pointer
/// can't record them.
fn collect_clean_extensions(cwd: &std::path::Path) -> Vec<CleanExtension> {
    git_lfs_git::extension::list_extensions(cwd)
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

/// The configured pointer extensions that have a smudge command.
///
/// Extensions with a priority outside 0 to 9 are skipped, as in
/// [`collect_clean_extensions`].
fn collect_smudge_extensions(cwd: &std::path::Path) -> Vec<SmudgeExtension> {
    git_lfs_git::extension::list_extensions(cwd)
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
    canonicalize_path_envs(&cwd);

    // Delete leftover temporary objects. Upstream does this when a
    // command exits instead.
    if let Ok(lfs_root) = git_lfs_git::lfs_dir(&cwd) {
        Store::new(lfs_root).cleanup_tmp_objects();
    }

    match cmd {
        Command::Clean(CleanArgs { path }) => {
            let _ = install::try_install_hooks(&cwd);
            let mut store = Store::new(git_lfs_git::lfs_dir(&cwd)?);
            if let Some(v) = shared_repo_config(&cwd) {
                store = store.with_shared_repository(&v);
            }
            // No `with_references`: clean only writes to the store.
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
            let mut store = Store::new(git_lfs_git::lfs_dir(&cwd)?)
                .with_references(git_lfs_git::lfs_alternate_dirs(&cwd).unwrap_or_default());
            if let Some(v) = shared_repo_config(&cwd) {
                store = store.with_shared_repository(&v);
            }
            let stdin = io::stdin().lock();
            let mut input: Box<dyn Read> = Box::new(stdin);
            let mut output: Box<dyn Write> = Box::new(BufWriter::new(io::stdout().lock()));
            if skip || skip_smudge_env() {
                io::copy(&mut input, &mut output)?;
            } else {
                let mut fetcher = smudge_fetcher::SmudgeFetcher::new(&cwd, &store);
                let smudge_extensions = collect_smudge_extensions(&cwd);
                let path_str = path
                    .as_deref()
                    .map(|p| p.to_string_lossy().into_owned())
                    .unwrap_or_default();
                // A path that `lfs.fetchinclude` or `lfs.fetchexclude`
                // filters out gets the pointer, as in filter-process.
                let include_set = fetch::build_pattern_set(&cwd, &[], "lfs.fetchinclude")?;
                let exclude_set = fetch::build_pattern_set(&cwd, &[], "lfs.fetchexclude")?;
                let filter_passes = path
                    .as_deref()
                    .is_none_or(|p| fetch::path_passes_filter(Some(p), &include_set, &exclude_set));
                // Buffer the input so the pointer can be written out if
                // the download fails and `lfs.skipdownloaderrors` is
                // set. Input that isn't a pointer is buffered whole.
                let mut buf = Vec::new();
                io::copy(&mut input, &mut buf)?;
                if !filter_passes {
                    output.write_all(&buf)?;
                } else {
                    let result = smudge_with_fetch(
                        &store,
                        &mut buf.as_slice(),
                        &mut output,
                        &path_str,
                        &smudge_extensions,
                        |p| fetcher.fetch_plain(p),
                    );
                    match result {
                        Ok(_) => {}
                        Err(e @ SmudgeError::FetchFailed { .. }) => {
                            // Like upstream, write the pointer instead.
                            output.write_all(&buf)?;
                            output.flush()?;
                            if let Some(line) = e.download_error_line() {
                                eprintln!("{line}");
                            }
                            if !env::skip_download_errors(&cwd) {
                                return Ok(2);
                            }
                        }
                        Err(e) => return Err(Box::new(e)),
                    }
                    fetcher.persist_access_mode();
                }
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
            manual,
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
            // The same condition `install::install` uses for hooks.
            let hooks_active =
                !skip_repo && (scope.is_repo_scope() || git_lfs_git::git_dir(&cwd).is_ok());
            if hooks_active && force && manual {
                eprintln!("{}", update::FORCE_AND_MANUAL);
                return Ok(2);
            }
            if hooks_active {
                update::migrate_access(&cwd);
            }
            let opts = install::InstallOptions {
                scope,
                force,
                // With `--manual`, the hooks are printed instead.
                skip_repo: skip_repo || manual,
                skip_smudge,
            };
            match install::install(&cwd, &opts) {
                Ok(()) => {
                    if hooks_active && manual {
                        update::print_manual(&cwd)?;
                    } else if hooks_active {
                        println!("Updated Git hooks.");
                    }
                    println!("Git LFS initialized.");
                }
                Err(e @ install::InstallError::FilterAttribute { .. }) => {
                    // On stdout, like upstream: t-install.sh only
                    // captures stdout.
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
            if install::uninstall(&cwd, &opts)? {
                println!("Hooks for this repository have been removed.");
            }
            if !hooks_only && let Some(message) = scope.removed_message() {
                println!("{message}");
            }
        }
        Command::Clone(CloneArgs { args }) => {
            match clone::run(&cwd, &args) {
                Ok(()) => {}
                // Upstream exits 2 with this, after the submodules'
                // own errors.
                Err(e @ clone::CloneError::Submodules(_)) => {
                    eprintln!("{e}");
                    return Ok(2);
                }
                Err(e) => return Err(e.into()),
            }
        }
        Command::FilterProcess(FilterProcessArgs { skip }) => {
            let _ = install::try_install_hooks(&cwd);
            let mut store = Store::new(git_lfs_git::lfs_dir(&cwd)?)
                .with_references(git_lfs_git::lfs_alternate_dirs(&cwd).unwrap_or_default());
            if let Some(v) = shared_repo_config(&cwd) {
                store = store.with_shared_repository(&v);
            }
            let mut fetcher = smudge_fetcher::SmudgeFetcher::new(&cwd, &store);
            let stdin = io::stdin().lock();
            let stdout = io::stdout().lock();
            let clean_extensions = collect_clean_extensions(&cwd);
            let smudge_extensions = collect_smudge_extensions(&cwd);
            // Paths that `lfs.fetchinclude` or `lfs.fetchexclude`
            // filters out are smudged to the pointer, as upstream does.
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
                |p, request| fetcher.fetch(p, request.treeish),
                skip || skip_smudge_env(),
                env::skip_download_errors(&cwd),
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
            recent,
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
                recent,
                include: &include,
                exclude: &exclude,
            };
            match fetch::fetch(&cwd, &opts) {
                Ok(outcome) => {
                    if !outcome.report.failed.is_empty() {
                        // Upstream exits 2 here, not 1.
                        return Err(format!(
                            "error: failed to fetch some objects from '{}'",
                            outcome.endpoint
                        )
                        .into());
                    }
                }
                Err(fetch::FetchCommandError::Usage(msg)) if msg == "Not in a Git repository." => {
                    // On stdout, like upstream: t-fetch.sh only captures
                    // stdout.
                    println!("{msg}");
                    return Ok(128);
                }
                Err(fetch::FetchCommandError::Prune(e)) if e.exits_2() => {
                    eprintln!("{e}");
                    return Ok(2);
                }
                Err(e) => return Err(e.into()),
            }
        }
        Command::Pull(PullArgs {
            args,
            include,
            exclude,
        }) => {
            match pull::pull_with_filter(&cwd, &args, &include, &exclude) {
                Ok(()) => {}
                Err(pull::PullCommandError::Fetch(fetch::FetchCommandError::Usage(msg)))
                    if msg == "Not in a Git repository." =>
                {
                    println!("{msg}");
                    return Ok(128);
                }
                Err(e @ pull::PullCommandError::FetchFailures(_)) => {
                    // Upstream exits 2 when objects fail to download,
                    // and t-pull.sh checks for that.
                    eprintln!("{e}");
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
        Command::StandaloneFile(StandaloneFileArgs) => {
            if let Some(code) = bail_if_outside_repo(&cwd) {
                return Ok(code);
            }
            standalone_file::run(&cwd)?;
        }
        Command::Dedup(DedupArgs { test }) => {
            if let Some(code) = bail_if_outside_repo(&cwd) {
                return Ok(code);
            }
            return dedup::run(&cwd, test);
        }
        Command::Completion(CompletionArgs { shell }) => {
            return Ok(completion::run(shell.as_deref()));
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
            no_extensions,
        }) => {
            let opts = pointer_cmd::Options {
                file,
                pointer,
                stdin,
                check,
                strict,
                no_strict,
                no_extensions,
                extensions: collect_clean_extensions(&cwd),
            };
            let code = pointer_cmd::run(&opts)?;
            return Ok(code as u8);
        }
        Command::Env(EnvArgs) => {
            env::run(&cwd)?;
        }
        Command::Ext(ExtArgs { cmd }) => match cmd {
            None => ext::run(&cwd)?,
            Some(ExtCmd::List(ExtListArgs { names })) => ext::run_list(&cwd, &names)?,
        },
        Command::Update(UpdateArgs { force, manual }) => match update::run(&cwd, force, manual) {
            Ok(code) => return Ok(code),
            Err(update::UpdateError::NotInRepo) => {
                // On stdout, like upstream: t-update.sh only captures
                // stdout.
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
                yes,
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
                    yes,
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
                    // `--fixup` looks for files that should be pointers
                    // but aren't, so it ignores pointers.
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
                    // A bare repository. Upstream exits 0 here too.
                    println!("This operation must be run in a work tree.");
                }
                Err(checkout::CheckoutError::NotInRepo) => {
                    println!("Not in a Git repository.");
                    return Ok(128);
                }
                Err(checkout::CheckoutError::Usage(msg)) => {
                    // Upstream's `Exit` prints to stderr and exits 2.
                    eprintln!("{msg}");
                    return Ok(2);
                }
                Err(e) => return Err(e.into()),
            }
        }
        Command::Prune(PruneArgs {
            dry_run,
            verbose,
            recent,
            force,
            verify_remote,
            no_verify_remote,
            verify_unreachable,
            no_verify_unreachable,
            when_unverified,
        }) => {
            let opts = prune::Options {
                dry_run,
                verbose,
                recent,
                force,
                verify_remote,
                no_verify_remote,
                verify_unreachable,
                no_verify_unreachable,
                continue_when_unverified: when_unverified == "continue",
            };
            match prune::run(&cwd, &opts) {
                Ok(()) => {}
                Err(e) if e.exits_2() => {
                    eprintln!("{e}");
                    return Ok(2);
                }
                Err(e) => return Err(e.into()),
            }
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
                    println!("Not in a Git repository.");
                    return Ok(128);
                }
                Err(status::StatusError::NotInWorkTree) => {
                    // A bare repository. Upstream exits 128 here.
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
            cached,
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
                cached,
                json,
            };
            match lock::locks(&cwd, &opts) {
                Ok(()) => {}
                Err(lock::LockCommandError::Usage(msg)) => {
                    eprintln!("{msg}");
                    return Ok(2);
                }
                Err(e) => return Err(e.into()),
            }
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
            refs,
            long,
            size,
            name_only,
            all,
            debug,
            deleted,
            include,
            exclude,
            json,
        }) => {
            if let Some(code) = bail_if_outside_repo(&cwd) {
                return Ok(code);
            }
            // Upstream's checks and messages, in its order.
            let usage_error = if all && !refs.is_empty() {
                Some("Cannot use --all with explicit reference")
            } else if refs.first().is_some_and(|r| r == "--all") {
                // `git lfs ls-files -- --all` would otherwise treat
                // `--all` as a ref.
                Some("Did you mean `git lfs ls-files --all --` ?")
            } else if deleted && refs.len() > 1 {
                Some("Cannot use --deleted with reference range")
            } else {
                None
            };
            if let Some(message) = usage_error {
                eprintln!("{message}");
                return Ok(2);
            }
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
                deleted,
                include: fetch::pattern_set(&include)?,
                exclude: fetch::pattern_set(&exclude)?,
                format,
            };
            ls_files::run(&cwd, &refs, &opts)?;
        }
        Command::MergeDriver(MergeDriverArgs {
            ancestor,
            current,
            other,
            output,
            program,
            marker_size,
        }) => {
            if let Some(code) = bail_if_outside_repo(&cwd) {
                return Ok(code);
            }
            let opts = merge_driver::MergeDriverOpts {
                ancestor: ancestor.as_deref(),
                current: current.as_deref(),
                other: other.as_deref(),
                output: output.as_deref(),
                program: program.as_deref(),
                marker_size,
            };
            match merge_driver::run(&cwd, &opts) {
                Ok(code) => return Ok(code),
                Err(merge_driver::MergeDriverError::MissingOptions) => {
                    eprintln!(
                        "the --ancestor, --current, --other, and --output options are mandatory"
                    );
                    return Ok(2);
                }
                Err(e) => return Err(Box::new(e)),
            }
        }
        Command::Logs(LogsArgs { sub }) => {
            if let Some(code) = bail_if_outside_repo(&cwd) {
                return Ok(code);
            }
            // The command line recorded in the boomtown log.
            let argv: Vec<String> = std::env::args().skip(1).collect();
            return match sub {
                None => Ok(logs::list(&cwd)?),
                Some(LogsSub::Last) => Ok(logs::last(&cwd)?),
                Some(LogsSub::Show { name }) => Ok(logs::show(&cwd, &name)?),
                Some(LogsSub::Clear) => Ok(logs::clear(&cwd)?),
                Some(LogsSub::Boomtown) => Ok(logs::boomtown(&cwd, &argv)?),
            };
        }
        Command::Untrack(UntrackArgs { patterns }) => {
            if patterns.is_empty() {
                return Err("git lfs untrack <pattern> [pattern...]".into());
            }
            // Like upstream, edit `.gitattributes` in the current
            // directory, or at the work tree root if `GIT_WORK_TREE`
            // points elsewhere. A bare repository fails like no
            // repository.
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
