//! `t-completion.sh`.

use std::path::Path;

use crate::harness::Test;

/// Check that `git lfs completion shell` prints the script in
/// `tests/fixtures/completions`.
#[track_caller]
fn assert_script(shell: &str) {
    let t = Test::new();
    let expected = Path::new(env!("CARGO_MANIFEST_DIR"))
        .join("../tests/fixtures/completions")
        .join(format!("git-lfs-completion.{shell}"));
    let expected = std::fs::read_to_string(expected).unwrap();

    let run = t.repo("").lfs(["completion", shell]).ok();

    assert!(run.stdout() == expected, "{shell} script differs");
}

#[test]
fn completion_bash_script() {
    assert_script("bash");
}

#[test]
fn completion_fish_script() {
    assert_script("fish");
}

#[test]
fn completion_zsh_script() {
    assert_script("zsh");
}

#[test]
fn completion_missing_shell_argument() {
    let t = Test::new();
    t.repo("")
        .lfs_command()
        .args(["completion"])
        .merged()
        .run()
        .has("accepts 1 arg");
}

#[test]
fn completion_invalid_shell_argument() {
    let t = Test::new();
    t.repo("")
        .lfs_command()
        .args(["completion", "ksh"])
        .merged()
        .run()
        .has("invalid argument");
}
