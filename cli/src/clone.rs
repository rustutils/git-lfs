//! `git lfs clone`: the deprecated wrapper around `git clone`.
//!
//! It runs `git clone` with the LFS filters turned off, like upstream's
//! `gitConfigNoLFS`, so the working tree gets pointer files. Then it runs
//! `git lfs pull` in the new repository, and `git lfs pull` in each
//! submodule if submodules were cloned, and installs the hooks unless
//! `--skip-repo` is given.
//!
//! With `--bare` or `--no-checkout`, the objects for the current ref are
//! fetched without a checkout, as in upstream.

use std::path::{Path, PathBuf};
use std::process::Command;

use crate::fetch;
use crate::install;
use crate::pull;

#[derive(Debug, thiserror::Error)]
pub enum CloneError {
    #[error("missing repository URL")]
    MissingRepo,
    #[error("`git clone` failed (exit {0})")]
    CloneFailed(i32),
    #[error("Unable to find clone dir at {0:?}")]
    MissingCloneDir(String),
    #[error(transparent)]
    Pull(#[from] pull::PullCommandError),
    #[error(transparent)]
    Fetch(#[from] fetch::FetchCommandError),
    /// `git lfs pull` failed in a submodule. Its errors were printed.
    #[error("Error performing `git lfs pull` for submodules: exit status {0}")]
    Submodules(i32),
    #[error(transparent)]
    Io(#[from] std::io::Error),
}

/// The command line split into what `git clone` gets and what this
/// command needs to know.
struct DecodedArgs {
    /// Arguments for `git clone`, which are all of them except
    /// `--include`, `--exclude`, and `--skip-repo`.
    forward: Vec<String>,
    /// The repository and the optional directory.
    positional: Vec<String>,
    no_checkout: bool,
    bare: bool,
    skip_repo: bool,
    /// `--recursive` or `--recurse-submodules`.
    recurse_submodules: bool,
    include: Vec<String>,
    exclude: Vec<String>,
}

pub fn run(cwd: &Path, raw_args: &[String]) -> Result<(), CloneError> {
    let decoded = decode_args(raw_args);

    if decoded.positional.is_empty() {
        return Err(CloneError::MissingRepo);
    }

    eprintln!("WARNING: `git lfs clone` is deprecated and will not be updated");
    eprintln!("          with new flags from `git clone`");
    eprintln!();
    eprintln!("`git clone` has been updated in upstream Git to have comparable");
    eprintln!("speeds to `git lfs clone`.");

    let mut cmd = Command::new("git");
    cmd.current_dir(cwd);
    for kv in [
        "filter.lfs.smudge=",
        "filter.lfs.clean=",
        "filter.lfs.process=",
        "filter.lfs.required=false",
    ] {
        cmd.args(["-c", kv]);
    }
    cmd.arg("clone");
    for a in &decoded.forward {
        cmd.arg(a);
    }
    let status = cmd.status()?;
    if !status.success() {
        return Err(CloneError::CloneFailed(status.code().unwrap_or(1)));
    }

    let clonedir = derive_clone_dir(cwd, &decoded.positional)?;
    // Like upstream, work from inside the clone. Child processes, such as
    // a custom transfer adapter for a local remote, start there too.
    std::env::set_current_dir(&clonedir)?;

    // An empty repository has no HEAD commit. Upstream skips the pull
    // then too.
    if has_resolvable_head(&clonedir) {
        if decoded.bare || decoded.no_checkout {
            // Without a checkout, only fetch, like upstream's `fetchRef`.
            // The index is empty, so the ref's tree is scanned instead.
            // Objects that fail are listed but don't fail the clone.
            let head = ["HEAD".to_owned()];
            let opts = fetch::FetchOptions {
                args: &head,
                stdin_lines: &[],
                dry_run: false,
                json: false,
                all: false,
                refetch: false,
                stdin: false,
                prune: false,
                recent: false,
                include: &decoded.include,
                exclude: &decoded.exclude,
            };
            fetch::fetch(&clonedir, &opts)?;
        } else {
            pull::pull_with_filter(&clonedir, &[], &decoded.include, &decoded.exclude)?;
            if decoded.recurse_submodules {
                // Git passes the `-c` options on to submodules, so they
                // have pointer files too.
                let status = Command::new("git")
                    .current_dir(&clonedir)
                    .args(["submodule", "foreach", "--recursive", "git lfs pull"])
                    .status()?;
                if !status.success() {
                    return Err(CloneError::Submodules(status.code().unwrap_or(1)));
                }
            }
        }
    }

    if !decoded.skip_repo {
        let _ = install::try_install_hooks(&clonedir);
    }

    Ok(())
}

/// Split the command line into the `git clone` arguments and the flags
/// this command handles.
fn decode_args(raw: &[String]) -> DecodedArgs {
    let mut forward = Vec::with_capacity(raw.len());
    let mut positional = Vec::new();
    let mut no_checkout = false;
    let mut bare = false;
    let mut skip_repo = false;
    let mut recurse_submodules = false;
    let mut include: Vec<String> = Vec::new();
    let mut exclude: Vec<String> = Vec::new();
    let mut after_dashdash = false;

    let mut i = 0;
    while i < raw.len() {
        let a = &raw[i];
        if after_dashdash {
            forward.push(a.clone());
            positional.push(a.clone());
            i += 1;
            continue;
        }
        if a == "--" {
            forward.push(a.clone());
            after_dashdash = true;
            i += 1;
            continue;
        }
        if a == "--skip-repo" {
            skip_repo = true;
            i += 1;
            continue;
        }
        if a == "-I" || a == "--include" {
            if let Some(v) = raw.get(i + 1) {
                include.push(v.clone());
            }
            i += 2;
            continue;
        }
        if let Some(v) = a.strip_prefix("--include=") {
            include.push(v.to_string());
            i += 1;
            continue;
        }
        if a == "-X" || a == "--exclude" {
            if let Some(v) = raw.get(i + 1) {
                exclude.push(v.clone());
            }
            i += 2;
            continue;
        }
        if let Some(v) = a.strip_prefix("--exclude=") {
            exclude.push(v.to_string());
            i += 1;
            continue;
        }
        if a == "--no-checkout" || a == "-n" {
            no_checkout = true;
        } else if a == "--bare" {
            bare = true;
        } else if a == "--recursive"
            || a == "--recurse-submodules"
            || a.starts_with("--recurse-submodules=")
        {
            recurse_submodules = true;
        }
        // Bundled short flags, such as `-lvn` or `-nbmain`.
        let mut short_value_follows = false;
        if let Some(rest) = a.strip_prefix('-')
            && !rest.starts_with('-')
        {
            let cluster = short_cluster(rest);
            no_checkout |= cluster.no_checkout;
            short_value_follows = cluster.value_follows;
        }
        if a.starts_with('-') {
            forward.push(a.clone());
            // Pass a flag's separate value through without taking it as
            // the repository or directory.
            if (value_taking_long(a) || short_value_follows) && i + 1 < raw.len() {
                forward.push(raw[i + 1].clone());
                i += 2;
                continue;
            }
            i += 1;
        } else {
            forward.push(a.clone());
            positional.push(a.clone());
            i += 1;
        }
    }
    DecodedArgs {
        forward,
        positional,
        no_checkout,
        bare,
        skip_repo,
        recurse_submodules,
        include,
        exclude,
    }
}

/// Whether `s` is a long `git clone` option whose value is the next
/// argument.
///
/// A missing option here makes its value look like the repository or
/// directory.
fn value_taking_long(s: &str) -> bool {
    if s.contains('=') {
        return false;
    }
    matches!(
        s,
        "--template"
            | "--origin"
            | "--branch"
            | "--upload-pack"
            | "--reference"
            | "--reference-if-able"
            | "--separate-git-dir"
            | "--depth"
            | "--config"
            | "--shallow-since"
            | "--shallow-exclude"
            | "--jobs"
            | "--server-option"
            | "--filter"
            | "--bundle-uri"
            | "--revision"
    )
}

/// What a cluster of short `git clone` options, after its `-`, says.
#[derive(Debug, PartialEq, Eq)]
struct ShortCluster {
    /// `n`, for `--no-checkout`, is one of the options.
    no_checkout: bool,
    /// The last option takes a value that's the next argument.
    value_follows: bool,
}

/// Read a cluster of short options as Git does.
///
/// The first option that takes a value, such as `b`, ends the cluster:
/// the rest is its value, as in `-bmain`, or the next argument is when
/// nothing's left.
fn short_cluster(rest: &str) -> ShortCluster {
    let mut no_checkout = false;
    for (i, c) in rest.char_indices() {
        match c {
            'o' | 'b' | 'u' | 'c' | 'j' => {
                return ShortCluster {
                    no_checkout,
                    value_follows: i + c.len_utf8() == rest.len(),
                };
            }
            'n' => no_checkout = true,
            _ => {}
        }
    }
    ShortCluster {
        no_checkout,
        value_follows: false,
    }
}

fn has_resolvable_head(cwd: &Path) -> bool {
    Command::new("git")
        .arg("-C")
        .arg(cwd)
        .args(["rev-parse", "--verify", "--quiet", "HEAD"])
        .output()
        .map(|o| o.status.success())
        .unwrap_or(false)
}

/// Find the new repository, like upstream.
///
/// That's the last positional argument if it's an existing directory,
/// and otherwise the repository's base name without `.git`.
fn derive_clone_dir(cwd: &Path, args: &[String]) -> Result<PathBuf, CloneError> {
    let last = args.last().ok_or(CloneError::MissingRepo)?;
    let abs_last = if Path::new(last).is_absolute() {
        PathBuf::from(last)
    } else {
        cwd.join(last)
    };
    if abs_last.is_dir() {
        return Ok(abs_last);
    }
    // Derive from the URL basename.
    let base = Path::new(last)
        .file_name()
        .map(|s| s.to_string_lossy().into_owned())
        .unwrap_or_default();
    let base = base.strip_suffix(".git").unwrap_or(&base);
    let derived = cwd.join(base);
    if derived.is_dir() {
        return Ok(derived);
    }
    Err(CloneError::MissingCloneDir(derived.display().to_string()))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn decode(args: &[&str]) -> DecodedArgs {
        decode_args(&args.iter().map(|s| s.to_string()).collect::<Vec<_>>())
    }

    #[test]
    fn a_value_in_a_short_option_is_not_more_options() {
        let d = decode(&["-bmain", "https://h/r.git", "dir"]);
        assert!(!d.no_checkout, "the n in main was read as -n");
        assert_eq!(d.positional, ["https://h/r.git", "dir"]);
    }

    #[test]
    fn short_options_bundle_like_git() {
        let d = decode(&["-qn", "https://h/r.git"]);
        assert!(d.no_checkout);

        // `b` takes the next argument when nothing's left after it.
        let d = decode(&["-nb", "main", "https://h/r.git"]);
        assert!(d.no_checkout);
        assert_eq!(d.positional, ["https://h/r.git"]);
        assert_eq!(d.forward, ["-nb", "main", "https://h/r.git"]);

        let d = decode(&["-b", "dev", "https://h/r.git"]);
        assert!(!d.no_checkout);
        assert_eq!(d.positional, ["https://h/r.git"]);
    }
}
