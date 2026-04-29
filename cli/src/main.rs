use std::io::{self, BufRead, BufWriter, Read, Write};
use std::process::ExitCode;

use clap::{CommandFactory, Parser};
use git_lfs_filter::{clean, filter_process, smudge_with_fetch};
use git_lfs_git::ConfigScope;
use git_lfs_store::Store;

mod checkout;
mod clone;
mod env;
mod fetch;
mod fetcher;
mod fsck;
mod hooks;
mod install;
mod lock;
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

use fetcher::LfsFetcher;

use git_lfs::args::{Cli, Command, MigrateCmd};

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

fn dispatch(cmd: Command) -> Result<u8, Box<dyn std::error::Error>> {
    let cwd = std::env::current_dir()?;

    match cmd {
        Command::Clean { path: _ } => {
            let _ = install::try_install_hooks(&cwd);
            let store = Store::new(git_lfs_git::lfs_dir(&cwd)?);
            let stdin = io::stdin().lock();
            let mut input: Box<dyn Read> = Box::new(stdin);
            let mut output: Box<dyn Write> = Box::new(BufWriter::new(io::stdout().lock()));
            clean(&store, &mut input, &mut output)?;
            output.flush()?;
        }
        Command::Smudge { path: _, skip } => {
            let _ = install::try_install_hooks(&cwd);
            let store = Store::new(git_lfs_git::lfs_dir(&cwd)?);
            let stdin = io::stdin().lock();
            let mut input: Box<dyn Read> = Box::new(stdin);
            let mut output: Box<dyn Write> = Box::new(BufWriter::new(io::stdout().lock()));
            if skip || skip_smudge_env() {
                io::copy(&mut input, &mut output)?;
            } else {
                let fetcher = LfsFetcher::from_repo(&cwd, &store)?;
                smudge_with_fetch(&store, &mut input, &mut output, |p| fetcher.fetch(p))?;
            }
            output.flush()?;
        }
        Command::Install {
            local,
            force,
            skip_repo,
            skip_smudge,
        } => {
            let opts = install::InstallOptions {
                scope: if local {
                    ConfigScope::Local
                } else {
                    ConfigScope::Global
                },
                force,
                skip_repo,
                skip_smudge,
            };
            install::install(&cwd, &opts)?;
            println!("Git LFS initialized.");
        }
        Command::Uninstall { local, skip_repo } => {
            let opts = install::UninstallOptions {
                scope: if local {
                    ConfigScope::Local
                } else {
                    ConfigScope::Global
                },
                skip_repo,
            };
            install::uninstall(&cwd, &opts)?;
            if local {
                println!("Local Git LFS configuration has been removed.");
            } else {
                println!("Global Git LFS configuration has been removed.");
            }
        }
        Command::Clone { args } => {
            clone::run(&cwd, &args)?;
        }
        Command::FilterProcess { skip } => {
            let _ = install::try_install_hooks(&cwd);
            let store = Store::new(git_lfs_git::lfs_dir(&cwd)?);
            let fetcher = LfsFetcher::from_repo(&cwd, &store)?;
            let stdin = io::stdin().lock();
            let stdout = io::stdout().lock();
            filter_process(
                &store,
                stdin,
                stdout,
                |p| fetcher.fetch(p),
                skip || skip_smudge_env(),
            )?;
        }
        Command::Fetch {
            args,
            dry_run,
            json,
            all,
            refetch,
            stdin,
            prune,
            include,
            exclude,
        } => {
            let stdin_lines: Vec<String> = if stdin {
                io::stdin()
                    .lock()
                    .lines()
                    .filter_map(|l| l.ok())
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
        Command::Pull {
            refs,
            include,
            exclude,
        } => {
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
                Err(e) => return Err(e.into()),
            }
        }
        Command::Push {
            remote,
            args,
            dry_run,
            all,
            stdin,
            object_id,
        } => {
            let stdin_lines: Vec<String> = if stdin {
                io::stdin()
                    .lock()
                    .lines()
                    .filter_map(|l| l.ok())
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
        Command::PostCheckout { args } => {
            hooks::post_checkout(&cwd, &args)?;
        }
        Command::PostCommit { args } => {
            hooks::post_commit(&cwd, &args)?;
        }
        Command::PostMerge { args } => {
            hooks::post_merge(&cwd, &args)?;
        }
        Command::PrePush {
            remote,
            url: _,
            dry_run,
        } => {
            let stdin = io::stdin().lock();
            let outcome = pre_push::pre_push(&cwd, &remote, stdin, dry_run)?;
            if outcome.aborted {
                return Ok(2);
            }
            if !outcome.report.failed.is_empty() {
                return Err("pre-push: one or more objects failed to upload".into());
            }
        }
        Command::Track {
            patterns,
            lockable,
            not_lockable,
            dry_run,
            verbose,
            json,
            no_excluded,
        } => {
            return track_cmd::run(track_cmd::Args {
                cwd: &cwd,
                patterns: &patterns,
                lockable,
                not_lockable,
                dry_run,
                verbose,
                json,
                no_excluded,
            });
        }
        Command::Version => {
            println!("git-lfs/{} (rust)", env!("CARGO_PKG_VERSION"));
        }
        Command::Pointer {
            file,
            pointer,
            stdin,
            check,
            strict,
            no_strict,
        } => {
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
        Command::Env => {
            env::run(&cwd)?;
        }
        Command::Migrate { cmd } => match cmd {
            MigrateCmd::Export {
                branches,
                everything,
                include,
                exclude,
            } => {
                let opts = migrate::ExportOptions {
                    branches,
                    everything,
                    include,
                    exclude,
                };
                migrate::export(&cwd, &opts)?;
            }
            MigrateCmd::Import {
                args,
                everything,
                include,
                exclude,
                above,
                no_rewrite,
                message,
            } => {
                let above_bytes = migrate::parse_size(&above)?;
                // Split: in --no-rewrite mode the positional args are
                // working-tree paths; otherwise they're branches.
                let (branches, paths) = if no_rewrite {
                    (Vec::new(), args)
                } else {
                    (args, Vec::new())
                };
                let opts = migrate::ImportOptions {
                    branches,
                    everything,
                    include,
                    exclude,
                    above: above_bytes,
                    no_rewrite,
                    message,
                    paths,
                };
                let _ = install::try_install_hooks(&cwd);
                migrate::import(&cwd, &opts)?;
            }
            MigrateCmd::Info {
                branches,
                everything,
                include,
                exclude,
                above,
                top,
                pointers,
            } => {
                let pointer_mode = match pointers.as_str() {
                    "follow" => migrate::PointerMode::Follow,
                    "no-follow" => migrate::PointerMode::NoFollow,
                    "ignore" => migrate::PointerMode::Ignore,
                    other => return Err(format!("--pointers: unknown value {other:?}").into()),
                };
                let above_bytes = migrate::parse_size(&above)?;
                let opts = migrate::InfoOptions {
                    branches,
                    everything,
                    include,
                    exclude,
                    above: above_bytes,
                    top,
                    pointers: pointer_mode,
                };
                migrate::info(&cwd, &opts)?;
            }
        },
        Command::Checkout { paths } => {
            let opts = checkout::Options { paths };
            match checkout::run(&cwd, &opts) {
                Ok(()) => {}
                Err(checkout::CheckoutError::NotInWorkTree) => {
                    // Bare repo: matches t-status / t-checkout
                    // bare-repo expectation. Exit 0 with the
                    // upstream-compatible message.
                    println!("This operation must be run in a work tree.");
                }
                Err(e) => return Err(e.into()),
            }
        }
        Command::Prune { dry_run, verbose } => {
            let opts = prune::Options { dry_run, verbose };
            prune::run(&cwd, &opts)?;
        }
        Command::Fsck {
            refspec,
            objects,
            pointers,
            dry_run,
        } => {
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
        Command::Status { porcelain, json } => {
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
        Command::Lock {
            paths,
            remote,
            refspec,
            json,
        } => {
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
        Command::Locks {
            remote,
            path,
            id,
            limit,
            refspec,
            verify,
            json,
        } => {
            let opts = lock::LocksOptions {
                remote,
                refspec,
                path,
                id,
                limit,
                verify,
                json,
            };
            lock::locks(&cwd, &opts)?;
        }
        Command::Unlock {
            paths,
            id,
            force,
            remote,
            refspec,
            json,
        } => {
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
        Command::LsFiles {
            refspec,
            long,
            size,
            name_only,
            all,
            debug,
            json,
        } => {
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
        Command::Untrack { patterns } => {
            if patterns.is_empty() {
                return Err("git lfs untrack <pattern> [pattern...]".into());
            }
            let _ = install::try_install_hooks(&cwd);
            let outcome = track::untrack(&cwd, &patterns)?;
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
