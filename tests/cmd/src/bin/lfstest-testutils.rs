//! Rust port of upstream's `tests/cmd/lfstest-testutils.go` — the
//! shell test framework's "make me some commits" helper.
//!
//! Only `addcommits` is implemented; that's the single subcommand the
//! vendored `t-*.sh` suite invokes.
//!
//! It reads a JSON array of commit specs on stdin, walks them in order,
//! and for each: optionally checks out / merges branches, writes one
//! LFS-pointer-shaped file per `Files[]` entry (storing the underlying
//! bytes in `.git/lfs/objects/`), `git add`s them, and creates a commit
//! at the requested timestamp. Tags are appended after the commit.
//! Output is JSON: an array of `{ Sha, Parents, Files[] }` for each
//! commit, written to stdout (the upstream Go callers don't actually
//! consume it, but tests pipe through `cat`-like steps that expect
//! valid JSON).

use std::io::{self, Read};
use std::path::Path;
use std::process::Command;

use git_lfs_pointer::Pointer;
use git_lfs_store::Store;
use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};

#[derive(Debug, Default, Deserialize)]
#[serde(default, rename_all = "PascalCase")]
struct CommitInput {
    commit_date: String,
    files: Vec<FileInput>,
    parent_branches: Vec<String>,
    new_branch: String,
    tags: Vec<String>,
    committer_name: String,
    committer_email: String,
}

#[derive(Debug, Default, Deserialize)]
#[serde(default, rename_all = "PascalCase")]
struct FileInput {
    filename: String,
    size: u64,
    data: String,
}

#[derive(Debug, Serialize)]
#[serde(rename_all = "PascalCase")]
struct CommitOutput {
    sha: String,
    parents: Vec<String>,
    files: Vec<PointerOut>,
}

#[derive(Debug, Serialize)]
#[serde(rename_all = "PascalCase")]
struct PointerOut {
    oid: String,
    size: u64,
}

fn main() {
    let mut args = std::env::args().skip(1);
    match args.next().as_deref() {
        Some("addcommits") => addcommits(),
        Some(cmd) => {
            eprintln!("Unknown command: {cmd}");
            std::process::exit(2);
        }
        None => {
            eprintln!("Command required (e.g. addcommits)");
            std::process::exit(2);
        }
    }
}

fn addcommits() {
    let mut buf = String::new();
    if let Err(e) = io::stdin().read_to_string(&mut buf) {
        eprintln!("addcommits: Unable to read input data: {e}");
        std::process::exit(3);
    }
    let inputs: Vec<CommitInput> = match serde_json::from_str(&buf) {
        Ok(v) => v,
        Err(e) => {
            eprintln!("addcommits: Unable to unmarshal JSON: {buf}\n{e}");
            std::process::exit(3);
        }
    };

    let cwd = std::env::current_dir().expect("getcwd");
    if !cwd.join(".git").exists() {
        eprintln!(
            "You're in the wrong directory, should be in root of a test repo: \
             no .git in {}",
            cwd.display()
        );
        std::process::exit(2);
    }

    let lfs_dir = cwd.join(".git").join("lfs");
    let store = Store::new(&lfs_dir);
    std::fs::create_dir_all(&lfs_dir).ok();

    let mut last_branch = String::from("main");
    let mut outputs: Vec<CommitOutput> = Vec::with_capacity(inputs.len());

    for (i, input) in inputs.iter().enumerate() {
        if !input.parent_branches.is_empty() && input.parent_branches[0] != last_branch {
            run_git(&cwd, &["checkout", &input.parent_branches[0]], true);
            last_branch = input.parent_branches[0].clone();
        }

        if input.parent_branches.len() > 1 {
            // Merges may legitimately conflict; upstream tolerates failures
            // and lets the subsequent commit pick up the partial state.
            let mut args: Vec<&str> = vec![
                "merge",
                "--no-ff",
                "--no-commit",
                "--strategy-option=theirs",
            ];
            for b in &input.parent_branches[1..] {
                args.push(b);
            }
            run_git(&cwd, &args, false);
        } else if !input.new_branch.is_empty() {
            run_git(&cwd, &["checkout", "-b", &input.new_branch], true);
            last_branch = input.new_branch.clone();
        }

        let mut file_outs: Vec<PointerOut> = Vec::with_capacity(input.files.len());
        for fi in &input.files {
            let bytes = if !fi.data.is_empty() {
                fi.data.as_bytes().to_vec()
            } else {
                placeholder_bytes(&fi.filename, fi.size)
            };
            let mut cursor = io::Cursor::new(&bytes);
            let (oid, size) = match store.insert(&mut cursor) {
                Ok(v) => v,
                Err(e) => {
                    eprintln!("addcommits: store insert {}: {e}", fi.filename);
                    std::process::exit(3);
                }
            };

            let dest = cwd.join(&fi.filename);
            if let Some(parent) = dest.parent() {
                std::fs::create_dir_all(parent).ok();
            }
            let pointer = Pointer::new(oid, size).encode();
            if let Err(e) = std::fs::write(&dest, &pointer) {
                eprintln!("addcommits: write {}: {e}", fi.filename);
                std::process::exit(3);
            }
            run_git(&cwd, &["add", &fi.filename], true);
            file_outs.push(PointerOut {
                oid: oid.to_string(),
                size,
            });
        }

        let msg = format!("Test commit {i}");
        commit_at_date(
            &cwd,
            &input.commit_date,
            &input.committer_name,
            &input.committer_email,
            &msg,
        );

        for tag in &input.tags {
            run_git(&cwd, &["tag", "-a", "-m", "Added tag", tag], true);
        }

        let sha = capture_git(&cwd, &["rev-parse", "HEAD"]);
        let row = capture_git(&cwd, &["rev-list", "--parents", "-n1", "HEAD"]);
        let mut parents: Vec<String> = row.split_whitespace().map(String::from).collect();
        if !parents.is_empty() {
            parents.remove(0);
        }
        outputs.push(CommitOutput {
            sha: sha.trim().to_string(),
            parents,
            files: file_outs,
        });
    }

    match serde_json::to_string(&outputs) {
        Ok(s) => println!("{s}"),
        Err(e) => {
            eprintln!("addcommits: Unable to marshal output JSON: {e}");
            std::process::exit(3);
        }
    }
}

fn placeholder_bytes(filename: &str, size: u64) -> Vec<u8> {
    // Deterministic stream — different filenames at the same size get
    // different OIDs. Upstream uses a global PRNG, but tests only check
    // file sizes, not specific OIDs, so any deterministic seed-by-name
    // scheme is enough.
    let mut hasher = Sha256::new();
    hasher.update(filename.as_bytes());
    let seed: [u8; 32] = hasher.finalize().into();
    let n = size as usize;
    let mut out = Vec::with_capacity(n);
    for i in 0..n {
        out.push(seed[i % 32] ^ ((i & 0xff) as u8));
    }
    out
}

fn run_git(cwd: &Path, args: &[&str], check: bool) {
    let status = Command::new("git")
        .args(args)
        .current_dir(cwd)
        .status()
        .expect("spawn git");
    if check && !status.success() {
        eprintln!(
            "Error running git command 'git {}': {status}",
            args.join(" ")
        );
        std::process::exit(4);
    }
}

fn capture_git(cwd: &Path, args: &[&str]) -> String {
    let out = Command::new("git")
        .args(args)
        .current_dir(cwd)
        .output()
        .expect("spawn git");
    if !out.status.success() {
        eprintln!(
            "Error running git command 'git {}': {}",
            args.join(" "),
            String::from_utf8_lossy(&out.stderr)
        );
        std::process::exit(4);
    }
    String::from_utf8_lossy(&out.stdout).to_string()
}

fn commit_at_date(cwd: &Path, date: &str, name: &str, email: &str, msg: &str) {
    let mut cmd = Command::new("git");
    cmd.current_dir(cwd);
    if !name.is_empty() && !email.is_empty() {
        cmd.args([
            "-c",
            &format!("user.name={name}"),
            "-c",
            &format!("user.email={email}"),
        ]);
    }
    cmd.args(["commit", "--allow-empty", "-m", msg]);
    if !date.is_empty() {
        cmd.env("GIT_COMMITTER_DATE", date);
        cmd.env("GIT_AUTHOR_DATE", date);
    }
    let status = cmd.status().expect("spawn git");
    if !status.success() {
        eprintln!("Error committing: {status}");
        std::process::exit(4);
    }
}
