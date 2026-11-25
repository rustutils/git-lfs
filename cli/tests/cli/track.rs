//! `t-track.sh`.

use crate::harness::{Repo, Run, Test, git_lfs, oid};

/// Check that `count` lines of `.gitattributes` have `ext` followed by
/// `attribute`, like the shell suites' `assert_attributes_count`.
#[track_caller]
fn assert_attributes_count(repo: &Repo, ext: &str, attribute: &str, count: usize) {
    let attributes = repo.read(".gitattributes");
    let actual = attributes
        .lines()
        .filter(|line| {
            line.find(ext)
                .is_some_and(|i| line[i + ext.len()..].contains(attribute))
        })
        .count();
    assert_eq!(
        actual, count,
        "{attribute} entries for {ext} in:\n{attributes}"
    );
}

/// Check that stdout is `expected`, give or take a final newline, as the
/// shell's `$(...)` compares it.
#[track_caller]
fn assert_output(run: &Run, expected: &str) {
    assert_eq!(run.stdout().trim_end_matches('\n'), expected, "{run}");
}

/// Check that `git status --porcelain` lists only untracked files.
#[track_caller]
fn assert_nothing_staged(repo: &Repo) {
    let run = repo.git(["status", "--porcelain"]).ok();
    let staged = run
        .stdout()
        .lines()
        .filter(|l| !l.starts_with("??"))
        .count();
    assert_eq!(staged, 0, "{run}");
}

#[test]
fn track() {
    let t = Test::new();
    let repo = t.init("track");
    repo.write(
        ".gitattributes",
        "###############################################################################
# Set default behavior to automatically normalize line endings.
###############################################################################
* text=auto

#*.cs     diff=csharp
",
    );

    repo.lfs(["track", "*.jpg"]).ok().has(r#"Tracking "*.jpg""#);
    assert_attributes_count(&repo, "jpg", "filter=lfs", 1);
    repo.lfs(["track", "*.jpg"])
        .ok()
        .has(r#""*.jpg" already supported"#);
    assert_attributes_count(&repo, "jpg", "filter=lfs", 1);

    repo.write(".git/info/attributes", "*.mov filter=lfs -text\n");
    repo.write("a/.gitattributes", "*.gif filter=lfs -text\n");
    repo.write("a/b/.gitattributes", "*.png filter=lfs -text\n");
    repo.lfs(["track"])
        .ok()
        .has("Listing tracked patterns")
        .has("*.mov (.git/info/attributes)")
        .has("*.jpg (.gitattributes)")
        .has("*.gif (a/.gitattributes)")
        .has("*.png (a/b/.gitattributes)");

    // The existing lines are kept.
    let attributes = repo.read(".gitattributes");
    for text in [
        "Set default behavior",
        "############",
        "* text=auto",
        "diff=csharp",
        "*.jpg",
    ] {
        assert!(attributes.contains(text), "no {text:?} in:\n{attributes}");
    }

    repo.append("a/b/.gitattributes", "*.gif -filter -text\n");
    repo.append("a/b/.gitattributes", "*.mov -filter -text\n");
    let run = repo.lfs(["track"]).ok();
    let stdout = run.stdout();
    let last: Vec<&str> = stdout.lines().rev().take(3).collect();
    assert!(last[2].contains("Listing excluded patterns"), "{run}");
    for pattern in ["*.gif", "*.mov"] {
        let line = format!("{pattern} (a/b/.gitattributes)");
        assert!(last.iter().any(|l| l.contains(&line)), "{run}");
    }
}

#[test]
fn track_no_excluded() {
    let t = Test::new();
    let repo = t.init("track_no_excluded");
    repo.write(".git/info/attributes", "*.mov filter=lfs -text\n");
    repo.write("a/.gitattributes", "*.gif filter=lfs -text\n");
    repo.write(
        "a/b/.gitattributes",
        "*.png filter=lfs -text\n*.gif -filter -text\n*.mov -filter=lfs -text\n",
    );
    repo.lfs(["track", "--no-excluded"])
        .ok()
        .lacks("Listing excluded patterns");
}

/// A new repository `name` with an empty `foo.dat` added.
fn foo_added<'a>(t: &'a Test, name: &str) -> Repo<'a> {
    let repo = t.init(name);
    repo.write("foo.dat", "");
    repo.git(["add", "foo.dat"]).ok();
    repo
}

#[test]
fn track_verbose() {
    let t = Test::new();
    let repo = foo_added(&t, "track_verbose_logs");
    let run = repo.lfs(["track", "--verbose", "foo.dat"]).ok();
    assert!(run.stdout().contains(r#"Touching "foo.dat""#), "{run}");
}

#[test]
fn track_dry_run() {
    let t = Test::new();
    let repo = foo_added(&t, "track_dry_run");
    let run = repo.lfs(["track", "--dry-run", "foo.dat"]).ok();
    let stdout = run.stdout();
    assert!(stdout.contains(r#"Tracking "foo.dat""#), "{run}");
    assert!(stdout.contains(r#"Touching "foo.dat""#), "{run}");
    repo.git(["status", "--porcelain"]).ok().has("A  foo.dat");
}

#[test]
fn track_directory() {
    let t = Test::new();
    let repo = t.init("dir");
    let run = repo.lfs(["track", "foo bar/*"]).ok();
    assert_output(&run, r#"Tracking "foo bar/*""#);
    assert_eq!(
        repo.read(".gitattributes"),
        "foo[[:space:]]bar/* filter=lfs diff=lfs merge=lfs -text\n"
    );

    repo.write("foo bar/a", "a\n");
    repo.write("foo bar/b", "b\n");
    repo.git(["add", "foo bar"]).ok();
    repo.git(["commit", "-am", "add foo bar"]).ok();
    repo.assert_pointer("main", "foo bar/a", &oid("a\n"), 2);
    repo.assert_pointer("main", "foo bar/b", &oid("b\n"), 2);
}

#[test]
fn track_without_trailing_linebreak() {
    let t = Test::new();
    let repo = t.init("no-linebreak");
    repo.write(".gitattributes", "*.mov filter=lfs -text");
    repo.lfs(["track", "*.gif"]).ok();
    assert_eq!(
        repo.read(".gitattributes"),
        "*.mov filter=lfs -text\n*.gif filter=lfs diff=lfs merge=lfs -text\n"
    );
}

#[test]
fn track_with_existing_crlf() {
    let t = Test::new();
    let repo = t.init("existing-crlf");
    repo.git(["config", "core.autocrlf", "true"]).ok();
    repo.lfs(["track", "*.mov"]).ok();
    repo.lfs(["track", "*.gif"]).ok();
    let crlf = "*.mov filter=lfs diff=lfs merge=lfs -text\r\n\
                *.gif filter=lfs diff=lfs merge=lfs -text\r\n";
    assert_eq!(repo.read(".gitattributes"), crlf);

    // The file's line endings win over the setting.
    repo.git(["config", "core.autocrlf", "false"]).ok();
    repo.lfs(["track", "*.jpg"]).ok();
    assert_eq!(
        repo.read(".gitattributes"),
        format!("{crlf}*.jpg filter=lfs diff=lfs merge=lfs -text\r\n")
    );
}

#[test]
fn track_with_autocrlf_true() {
    let t = Test::new();
    let repo = t.init("autocrlf-true");
    repo.git(["config", "core.autocrlf", "true"]).ok();
    repo.write(".gitattributes", "*.mov filter=lfs -text");
    repo.lfs(["track", "*.gif"]).ok();
    assert_eq!(
        repo.read(".gitattributes"),
        "*.mov filter=lfs -text\r\n*.gif filter=lfs diff=lfs merge=lfs -text\r\n"
    );
}

#[test]
fn track_with_autocrlf_input() {
    let t = Test::new();
    let repo = t.init("autocrlf-input");
    repo.git(["config", "core.autocrlf", "input"]).ok();
    repo.write(".gitattributes", "*.mov filter=lfs -text");
    repo.lfs(["track", "*.gif"]).ok();
    let attributes = repo.read(".gitattributes");
    assert!(!attributes.contains('\r'), "{attributes:?}");
}

/// Outside a repository, track exits 128, as Git does. The shell test
/// goes on to expect the same for `../*.foo` inside one, but only reaches
/// that when the suite itself runs in a repository, and upstream tracks
/// the pattern there.
#[test]
fn track_outside_git_repo() {
    let t = Test::new();
    let run = t.command(t.dir(), git_lfs()).args(["track", "*.foo"]).run();
    assert_eq!(run.code(), Some(128), "{run}");
}

#[test]
fn track_representation() {
    let t = Test::new();
    let repo = t.init("track-representation");
    repo.lfs(["track", "*.jpg"]).ok();
    std::fs::create_dir(repo.path().join("a")).unwrap();
    repo.lfs(["track", "a/test.file"]).ok();

    let sub = repo.at("a");
    let run = sub.lfs(["track", "test.file"]).ok();
    assert_output(&run, r#""test.file" already supported"#);
    sub.lfs(["track", "file.bin"]).ok();
    let run = repo.lfs(["track", "a/file.bin"]).ok();
    assert_output(&run, r#""a/file.bin" already supported"#);
}

#[test]
fn track_absolute() {
    let t = Test::new();
    let repo = t.init("track-absolute");
    repo.lfs(["track", "/images"]).ok();
    let attributes = repo.read(".gitattributes");
    assert!(attributes.starts_with("/images"), "{attributes}");
}

#[test]
fn track_in_git_dir() {
    let t = Test::new();
    let repo = t.init("track-in-dot-git");
    repo.write("test.file", "some content\n");
    let run = repo.at(".git").lfs(["track", "../test.file"]);
    assert_eq!(run.code(), Some(128), "{run}");
}

#[test]
fn track_in_symlinked_dir() {
    let t = Test::new();
    t.init("track-symlinkdst");
    std::os::unix::fs::symlink("track-symlinkdst", t.dir().join("track-symlinksrc")).unwrap();
    let repo = t.repo("track-symlinksrc");
    repo.lfs(["track", "*.png"]).ok();
    let attributes = repo.read(".gitattributes");
    assert!(attributes.starts_with("*.png"), "{attributes}");
}

/// A new repository `name` with an empty `.gitattributes` committed.
fn attributes_committed<'a>(t: &'a Test, name: &str) -> Repo<'a> {
    let repo = t.init(name);
    repo.write(".gitattributes", "");
    repo.git(["add", ".gitattributes"]).ok();
    repo.git(["commit", "-m", "Initial commit"]).ok();
    repo
}

/// Check that tracking `pattern` fails with `message` on stdout, and
/// leaves nothing staged.
#[track_caller]
fn assert_forbidden(repo: &Repo, pattern: &str, message: &str) {
    let run = repo.lfs(["track", pattern]).fails();
    assert!(run.stdout().contains(message), "{run}");
    assert_nothing_staged(repo);
}

#[test]
fn track_blocklisted_files_by_name() {
    let t = Test::new();
    let repo = attributes_committed(&t, "track_blocklisted_by_name");
    assert_forbidden(
        &repo,
        ".gitattributes",
        "Pattern '.gitattributes' matches forbidden file '.gitattributes'",
    );
}

#[test]
fn track_blocklisted_files_with_glob() {
    let t = Test::new();
    let repo = attributes_committed(&t, "track_blocklisted_glob");
    assert_forbidden(&repo, ".git*", "Pattern '.git*' matches forbidden file");
    assert_forbidden(&repo, "*", "Pattern '*' matches forbidden file");
}

#[test]
fn track_lockable() {
    let t = Test::new();
    let repo = t.init("track_lockable");
    let track = |args: &[&str], message: &str| {
        repo.lfs(["track"].iter().chain(args)).ok().has(message);
    };

    track(&["--lockable", "*.jpg"], r#"Tracking "*.jpg""#);
    assert_attributes_count(&repo, "jpg", "lockable", 1);
    // Tracking again changes nothing, so it stays lockable.
    track(&["*.jpg"], r#""*.jpg" already supported"#);
    assert_attributes_count(&repo, "jpg", "lockable", 1);

    // Each change of lockability replaces the line.
    for (args, lockable) in [
        (&["*.png"][..], 0),
        (&["--lockable", "*.png"], 1),
        (&["--not-lockable", "*.png"], 0),
    ] {
        track(args, r#"Tracking "*.png""#);
        assert_attributes_count(&repo, "png", "filter=lfs", 1);
        assert_attributes_count(&repo, "png", "lockable", lockable);
    }

    repo.lfs(["track"])
        .ok()
        .has("Listing tracked patterns")
        .has("*.jpg [lockable] (.gitattributes)")
        .has("*.png (.gitattributes)");
}

#[test]
fn track_lockable_read_only_read_write() {
    let t = Test::new();
    let repo = t.init("track_lockable_ro_rw");
    repo.write("test.bin", "blah blah\n");
    repo.write("test.dat", "foo bar\n");
    repo.write("subfolder/test.bin", "sub blah blah\n");
    repo.write("subfolder/test.dat", "sub foo bar\n");
    repo.git(["add", "test.bin", "test.dat", "subfolder"]).ok();
    // Which of the files are writeable: bin, then dat.
    let assert_writeable = |bin: bool, dat: bool| {
        for dir in ["", "subfolder/"] {
            for (ext, writeable) in [("bin", bin), ("dat", dat)] {
                let file = format!("{dir}test.{ext}");
                assert_eq!(repo.is_writeable(&file), writeable, "{file}");
            }
        }
    };
    assert_writeable(true, true);

    repo.lfs(["track", "*.bin"]).ok().has(r#"Tracking "*.bin""#);
    repo.lfs(["track", "--lockable", "*.dat"])
        .ok()
        .has(r#"Tracking "*.dat""#);
    assert_writeable(true, false);

    repo.git(["add", ".gitattributes", "test.bin", "test.dat"])
        .ok();
    repo.git(["commit", "-m", "First commit"]).ok();
    assert_writeable(true, false);

    repo.lfs(["track", "--lockable", "*.bin"])
        .ok()
        .has(r#"Tracking "*.bin""#);
    assert_writeable(false, false);
    repo.lfs(["track", "--not-lockable", "*.bin"])
        .ok()
        .has(r#"Tracking "*.bin""#);
    assert_writeable(true, false);
}

#[test]
fn track_escaped_pattern() {
    let t = Test::new();
    let repo = t.init("track-escaped-pattern");
    repo.lfs(["track", " "]).ok().has(r#"Tracking " ""#);
    assert_attributes_count(&repo, "[[:space:]]", "filter=lfs", 1);
    repo.lfs(["track", "#"]).ok().has(r##"Tracking "#""##);
    assert_attributes_count(&repo, "\\#", "filter=lfs", 1);
}

#[test]
fn track_symlinked_repository() {
    let t = Test::new();
    let name = "tracked-symlinked-repository";
    let repo = t.init(name);
    repo.write("a.dat", "");
    let dir = t.dir().join("tracked-symlinked-repository-tmp");
    std::fs::create_dir(&dir).unwrap();
    std::os::unix::fs::symlink(format!("../{name}"), dir.join(name)).unwrap();

    let linked = t.repo(&format!("tracked-symlinked-repository-tmp/{name}"));
    assert_output(&linked.lfs(["track", "a.dat"]).ok(), r#"Tracking "a.dat""#);
    assert_output(
        &linked.lfs(["track", "a.dat"]).ok(),
        r#""a.dat" already supported"#,
    );
}

#[test]
fn track_git_lfs_track_no_install_hooks() {
    let t = Test::new();
    let repo = t.init("track-no-setup-hooks");
    let assert_no_hooks = || {
        for hook in ["pre-push", "post-checkout", "post-commit", "post-merge"] {
            let path = repo.git_dir().join("hooks").join(hook);
            assert!(!path.exists(), "{}", path.display());
        }
    };
    assert_no_hooks();
    repo.lfs_command()
        .args(["track"])
        .env("GIT_LFS_TRACK_NO_INSTALL_HOOKS", "1")
        .run()
        .ok();
    assert_no_hooks();
}

#[test]
fn track_with_comments() {
    let t = Test::new();
    let repo = t.init("track-with=comments");
    repo.write(
        ".gitattributes",
        "*.jpg filter=lfs diff=lfs merge=lfs -text\n\
         # *.png filter=lfs diff=lfs merge=lfs -text\n\
         *.pdf filter=lfs diff=lfs merge=lfs -text\n",
    );
    repo.git(["add", ".gitattributes"]).ok();
    repo.git(["commit", "-m", "initial commit"]).ok();

    let run = repo.lfs(["track"]).ok();
    assert_eq!(run.count(".jpg"), 1, "{run}");
    assert_eq!(run.count(".pdf"), 1, "{run}");
    assert_eq!(run.count(".png"), 0, "{run}");
}

#[test]
fn track_with_current_directory_prefix() {
    let t = Test::new();
    let repo = t.init("track-with-current-directory-prefix");
    repo.lfs(["track", "./a.dat"]).ok();
    repo.write("a.dat", "a");
    repo.git(["add", ".gitattributes", "a.dat"]).ok();
    repo.git(["commit", "-m", "initial commit"]).ok();
    let attributes = repo.read(".gitattributes");
    assert!(attributes.starts_with("a.dat"), "{attributes}");
}

#[test]
fn track_global_gitattributes() {
    let t = Test::new();
    let repo = t.init("track-global-gitattributes");
    let global = t.dir().join("gitattributes-global");
    std::fs::write(&global, "*.dat filter=lfs diff=lfs merge=lfs -text\n").unwrap();
    repo.git([
        "config",
        "--local",
        "core.attributesfile",
        global.to_str().unwrap(),
    ])
    .ok();
    repo.lfs(["track"]).ok().has("*.dat");
}

/// Git before 2.42 reads `$PREFIX/etc/gitattributes`. Later versions don't,
/// so the test does nothing with them.
#[test]
fn track_system_gitattributes() {
    let t = Test::new();
    if t.git_version() >= (2, 42) {
        return;
    }
    let name = "track-system-gitattributes";
    let repo = t.init(name);
    let prefix = t.dir().join("prefix").join(name);
    std::fs::create_dir_all(prefix.join("etc")).unwrap();
    std::fs::write(
        prefix.join("etc/gitattributes"),
        "*.dat filter=lfs diff=lfs merge=lfs -text\n",
    )
    .unwrap();
    repo.lfs_command()
        .args(["track"])
        .env("PREFIX", &prefix)
        .run()
        .ok()
        .has("*.dat");
}

#[test]
fn track_escaped_pattern_in_gitattributes() {
    let t = Test::new();
    let repo = t.init("track-escaped");
    let file = "file with spaces.#";
    repo.write(file, "I need escaping\n");

    let tracking = format!(r#"Tracking "{file}""#);
    let supported = format!(r#""{file}" already supported"#);
    assert_output(&repo.lfs(["track", file]).ok(), &tracking);
    assert_output(&repo.lfs(["track", file]).ok(), &supported);
    // Changing the flags tracks it again, on the same line.
    assert_output(&repo.lfs(["track", "-l", file]).ok(), &tracking);
    assert_eq!(repo.read(".gitattributes").lines().count(), 1);

    let nested = format!("foo/bar/{file}");
    assert_output(
        &repo.lfs(["track", &nested]).ok(),
        &format!(r#"Tracking "{nested}""#),
    );
    assert_output(
        &repo.lfs(["track", &nested]).ok(),
        &format!(r#""{nested}" already supported"#),
    );
}

/// Track `file` by name, then commit it, and check Git stored it as a
/// pointer, so it understood the escaping.
fn track_filename(name: &str, file: &str, check: impl Fn(&Repo)) {
    let t = Test::new();
    let repo = t.init(name);
    check(&repo);
    repo.git(["add", "."]).ok();
    let contents = "I need escaping";
    repo.write(file, contents);
    repo.git(["add", "."]).ok();
    repo.git(["commit", "-m", "Add unusually named file"]).ok();
    repo.assert_pointer("main", file, &oid(contents), 15);
}

#[test]
fn track_escaped_glob_pattern_in_gitattributes() {
    let file = "[foo]bar.txt";
    track_filename("track-escaped-glob", file, |repo| {
        repo.lfs(["track", "--filename", file]).ok();
        repo.lfs(["track", "--filename", file])
            .ok()
            .has("already supported");
    });
}

/// The `\n` is a backslash and an `n`, not a newline.
#[test]
fn track_escaped_glob_pattern_with_spaces_in_gitattributes() {
    let file = r"*[foo] \n bar?.txt";
    track_filename("track-escaped-glob-spaces", file, |repo| {
        // `git ls-files` mishandling the backslash would give this error.
        repo.lfs(["track", "--filename", file])
            .ok()
            .lacks("Error marking");
    });
}

#[test]
fn track_verbose_logging() {
    let t = Test::new();
    let repo = t.init("track-verbose-logging");
    let file = "[foo]bar.bin";
    repo.write(file, "I need escaping");
    repo.write("a.txt", "Hello, world!\n");
    repo.git(["add", "a.txt", file]).ok();
    repo.git(["commit", "-m", "some files"]).ok();

    let found = "Found 1 files previously added to Git matching pattern:";
    repo.lfs(["track", "-v", "*.txt"]).ok().has(found);
    repo.lfs(["track", "-v", "--filename", file])
        .ok()
        .has(found);
}

#[test]
fn json_output() {
    let t = Test::new();
    let repo = t.init("track-json");
    repo.lfs(["track", "*.dat"]).ok();
    repo.lfs(["track", "--lockable", "*.bin"]).ok();
    repo.append(".gitattributes", "a.dat !filter\n");

    let pattern = |pattern: &str, lockable: bool, tracked: bool| {
        format!(
            "  {{\n   \"pattern\": \"{pattern}\",\n   \"source\": \".gitattributes\",\n   \"lockable\": {lockable},\n   \"tracked\": {tracked}\n  }}"
        )
    };
    let expected = format!(
        "{{\n \"patterns\": [\n{},\n{},\n{}\n ]\n}}\n",
        pattern("*.dat", false, true),
        pattern("*.bin", true, true),
        pattern("a.dat", false, false),
    );
    let run = repo.lfs(["track", "--json"]).ok();
    assert_eq!(run.stdout(), expected, "{run}");
}
