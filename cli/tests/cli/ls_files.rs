//! `t-ls-files.sh`.

use crate::harness::{Repo, Run, Test, git_lfs, oid};

/// An LFS file as `ls-files` reports it.
struct Entry {
    name: &'static str,
    contents: &'static str,
    checkout: bool,
    downloaded: bool,
}

impl Entry {
    fn new(name: &'static str, contents: &'static str) -> Self {
        Self {
            name,
            contents,
            checkout: true,
            downloaded: true,
        }
    }

    /// Neither in the working tree nor, if `downloaded` is false, in the
    /// local store.
    fn removed(mut self, downloaded: bool) -> Self {
        self.checkout = false;
        self.downloaded = downloaded;
        self
    }

    /// The default output's line: `*` when checked out, `-` when not.
    fn line(&self) -> String {
        let mark = if self.checkout { '*' } else { '-' };
        format!("{} {mark} {}", &oid(self.contents)[..10], self.name)
    }

    fn debug(&self) -> String {
        format!(
            "filepath: {}\n    size: {}\ncheckout: {}\ndownload: {}\n     oid: sha256 {}\n version: https://git-lfs.github.com/spec/v1\n\n",
            self.name,
            self.contents.len(),
            self.checkout,
            self.downloaded,
            oid(self.contents),
        )
    }

    fn json(&self) -> String {
        format!(
            "  {{\n   \"name\": \"{}\",\n   \"size\": {},\n   \"checkout\": {},\n   \"downloaded\": {},\n   \"oid_type\": \"sha256\",\n   \"oid\": \"{}\",\n   \"version\": \"https://git-lfs.github.com/spec/v1\"\n  }}",
            self.name,
            self.contents.len(),
            self.checkout,
            self.downloaded,
            oid(self.contents),
        )
    }
}

/// Check the default, `--debug`, and `--json` output against `entries`.
#[track_caller]
fn assert_ls_files(repo: &Repo, entries: &[Entry]) {
    let lines: Vec<String> = entries.iter().map(Entry::line).collect();
    assert_output(&repo.lfs(["ls-files"]).ok(), &lines.join("\n"));
    let debug: String = entries.iter().map(Entry::debug).collect();
    let run = repo.lfs(["ls-files", "--debug"]).ok();
    assert_eq!(run.stdout(), debug, "{run}");
    assert_json(repo, entries);
}

#[track_caller]
fn assert_json(repo: &Repo, entries: &[Entry]) {
    let files: Vec<String> = entries.iter().map(Entry::json).collect();
    let json = format!("{{\n \"files\": [\n{}\n ]\n}}\n", files.join(",\n"));
    let run = repo.lfs(["ls-files", "--json"]).ok();
    assert_eq!(run.stdout(), json, "{run}");
}

/// Check that stdout is `expected`, give or take a final newline, as the
/// shell's `$(...)` compares it.
#[track_caller]
fn assert_output(run: &Run, expected: &str) {
    assert_eq!(run.stdout().trim_end_matches('\n'), expected, "{run}");
}

/// A new repository `name` with `*.dat` tracked and committed.
fn tracked<'a>(t: &'a Test, name: &str) -> Repo<'a> {
    let repo = t.init(name);
    repo.lfs(["track", "*.dat"]).ok();
    repo.git(["add", ".gitattributes"]).ok();
    repo.git(["commit", "-m", "initial commit"]).ok();
    repo
}

/// The count of output lines containing `text`.
fn count(run: &Run, text: &str) -> usize {
    run.output().lines().filter(|l| l.contains(text)).count()
}

/// `missing.dat` committed, then removed while `some.dat` and `some.txt`
/// are added. `ls-files` lists what HEAD has.
fn some_and_missing<'a>(t: &'a Test, name: &str) -> Repo<'a> {
    let repo = t.init(name);
    repo.lfs(["track", "*.dat"]).ok().has(r#"Tracking "*.dat""#);
    repo.write("some.dat", "some data\n");
    repo.write("some.txt", "some text\n");
    repo.write("missing.dat", "missing\n");
    repo.git(["add", "missing.dat"]).ok();
    repo.git(["commit", "-m", "add missing file"]).ok();
    repo
}

fn remove_missing(repo: &Repo) {
    repo.git(["rm", "missing.dat"]).ok();
    repo.git(["add", "some.dat", "some.txt"]).ok();
    repo.git(["commit", "-m", "added some files, removed missing one"])
        .ok();
}

#[test]
fn ls_files() {
    let t = Test::new();
    let repo = some_and_missing(&t, "repo");
    assert_output(&repo.lfs(["ls-files"]).ok(), "6bbd052ab0 * missing.dat");
    remove_missing(&repo);
    assert_ls_files(&repo, &[Entry::new("some.dat", "some data\n")]);
}

#[test]
fn ls_files_json() {
    let t = Test::new();
    let repo = some_and_missing(&t, "ls-files-json");
    assert_json(&repo, &[Entry::new("missing.dat", "missing\n")]);
    remove_missing(&repo);
    assert_json(&repo, &[Entry::new("some.dat", "some data\n")]);
}

/// `subdir/missing.dat` committed, then removed while `subdir/some.dat`
/// and `subdir/some.txt` are added.
fn files_in_subdirectory<'a>(t: &'a Test, name: &str) -> Repo<'a> {
    let repo = tracked(t, name);
    repo.write("subdir/missing.dat", "missing");
    repo.git(["add", "subdir"]).ok();
    repo.git(["commit", "-m", "add file in subdirectory"]).ok();
    repo.write("subdir/some.dat", "some data");
    repo.write("subdir/some.txt", "some text\n");
    let missing = format!("{} * subdir/missing.dat", &oid("missing")[..10]);
    assert_output(&repo.lfs(["ls-files"]).ok(), &missing);

    repo.git(["rm", "subdir/missing.dat"]).ok();
    repo.git(["add", "subdir"]).ok();
    repo.git(["commit", "-m", "add and remove files in subdirectory"])
        .ok();
    repo
}

#[test]
fn ls_files_files_in_subdirectory() {
    let t = Test::new();
    let repo = files_in_subdirectory(&t, "ls-files-subdir");
    assert_ls_files(&repo, &[Entry::new("subdir/some.dat", "some data")]);
}

#[test]
fn ls_files_files_in_subdirectory_json() {
    let t = Test::new();
    let repo = files_in_subdirectory(&t, "ls-files-subdir-json");
    assert_json(&repo, &[Entry::new("subdir/some.dat", "some data")]);
}

/// `a.dat` and `subdir/b.dat` added from `subdir`, which `ls-files` runs
/// in. Before they're added, it lists nothing.
fn run_within_subdirectory<'a>(t: &'a Test, name: &str) -> (Repo<'a>, [Entry; 2]) {
    let repo = tracked(t, name);
    repo.write("a.dat", "a");
    repo.write("subdir/b.dat", "b");
    let sub = repo.at("subdir");
    assert_output(&sub.lfs(["ls-files"]).ok(), "");
    sub.git(["add", "../a.dat", "b.dat"]).ok();
    let entries = [Entry::new("a.dat", "a"), Entry::new("subdir/b.dat", "b")];
    (t.repo(&format!("{name}/subdir")), entries)
}

#[test]
fn ls_files_run_within_subdirectory() {
    let t = Test::new();
    let (sub, entries) = run_within_subdirectory(&t, "ls-files-in-subdir");
    assert_ls_files(&sub, &entries);
}

#[test]
fn ls_files_run_within_subdirectory_json() {
    let t = Test::new();
    let (sub, entries) = run_within_subdirectory(&t, "ls-files-in-subdir-json");
    assert_json(&sub, &entries);
}

/// Files added and then deleted from the working tree, with every other
/// one's object deleted from the store too. `subdir` also adds two files
/// in a subdirectory, which `ls-files` then runs in.
fn status<'a>(t: &'a Test, name: &str, subdir: bool) -> (Repo<'a>, Vec<Entry>) {
    let repo = tracked(t, name);
    let mut files = vec![("a.dat", "a"), ("b.dat", "b")];
    if subdir {
        files.extend([("subdir/c.dat", "c"), ("subdir/d.dat", "d")]);
    }
    for (file, contents) in &files {
        repo.write(file, contents);
    }
    let cwd = if subdir {
        repo.at("subdir")
    } else {
        repo.at("")
    };
    assert_output(&cwd.lfs(["ls-files"]).ok(), "");

    let names: Vec<&str> = files.iter().map(|(f, _)| *f).collect();
    repo.git(["add"].iter().chain(&names)).ok();
    let mut entries = Vec::new();
    for (i, (file, contents)) in files.into_iter().enumerate() {
        std::fs::remove_file(repo.path().join(file)).unwrap();
        // Removed from the working tree too, so Git can't restore it
        // through the clean filter.
        let downloaded = i % 2 == 0;
        if !downloaded {
            repo.delete_local_object(&oid(contents));
        }
        entries.push(Entry::new(file, contents).removed(downloaded));
    }
    let dir = if subdir {
        format!("{name}/subdir")
    } else {
        name.to_owned()
    };
    (t.repo(&dir), entries)
}

#[test]
fn ls_files_checkout_and_download_status() {
    let t = Test::new();
    let (repo, entries) = status(&t, "ls-files-status", false);
    assert_ls_files(&repo, &entries);
}

#[test]
fn ls_files_checkout_and_download_status_run_within_subdirectory() {
    let t = Test::new();
    let (repo, entries) = status(&t, "ls-files-status-in-subdir", true);
    assert_ls_files(&repo, &entries);
}

#[test]
fn ls_files_checkout_and_download_status_json() {
    let t = Test::new();
    let (repo, entries) = status(&t, "ls-files-status-json", false);
    assert_json(&repo, &entries);
}

#[test]
fn ls_files_checkout_and_download_status_run_within_subdirectory_json() {
    let t = Test::new();
    let (repo, entries) = status(&t, "ls-files-status-in-subdir-json", true);
    assert_json(&repo, &entries);
}

#[test]
fn ls_files_size() {
    let t = Test::new();
    let repo = tracked(&t, "ls-files-size");
    repo.write("a.dat", "contents");
    repo.git(["add", "a.dat"]).ok();
    repo.git(["commit", "-m", "add a.dat"]).ok();
    let run = repo.lfs(["ls-files", "--size"]).ok();
    assert_eq!(run.output().trim_end(), "d1b2a59fbe * a.dat (8 B)", "{run}");
}

#[test]
fn ls_files_correctly_reflects_case_sensitivity() {
    let t = Test::new();
    let repo = t.init("ls-files-case-sensitive");
    let ignorecase = repo.git(["config", "core.ignorecase"]).stdout();
    repo.lfs(["track", "*.DAT"]).ok();
    repo.git(["add", ".gitattributes"]).ok();
    repo.write("a.dat", "a");
    assert_output(&repo.lfs(["ls-files"]).ok(), "");

    repo.git(["add", "a.dat"]).ok();
    let expected = if ignorecase.trim() == "true" {
        format!("{} * a.dat", &oid("a")[..10])
    } else {
        String::new()
    };
    assert_output(&repo.lfs(["ls-files"]).ok(), &expected);
}

#[test]
fn ls_files_indexed_files_without_tree() {
    let t = Test::new();
    let repo = t.init("ls-files-indexed-files-without-tree");
    repo.lfs(["track", "*.dat"]).ok();
    repo.git(["add", ".gitattributes"]).ok();
    repo.write("a.dat", "a");
    assert_output(&repo.lfs(["ls-files"]).ok(), "");
    repo.git(["add", "a.dat"]).ok();
    let expected = format!("{} * a.dat", &oid("a")[..10]);
    assert_output(&repo.lfs(["ls-files"]).ok(), &expected);
}

#[test]
fn ls_files_indexed_file_with_tree() {
    let t = Test::new();
    let repo = tracked(&t, "ls-files-indexed-files-with-tree");
    repo.write("a.dat", "a");
    repo.git(["add", "a.dat"]).ok();
    repo.git(["commit", "-m", "add a.dat"]).ok();
    repo.write("a.dat", "b");
    repo.git(["add", "a.dat"]).ok();
    let expected = format!("{} * a.dat", &oid("b")[..10]);
    assert_output(&repo.lfs(["ls-files"]).ok(), &expected);
}

/// `a.txt` committed and `b.txt` either committed after it or only
/// added.
fn txt_setup<'a>(t: &'a Test, name: &str) -> Repo<'a> {
    let repo = t.init(name);
    repo.lfs(["track", "*.txt"]).ok();
    for file in ["a.txt", "b.txt", "c.txt"] {
        repo.write(file, format!("{file}\n"));
    }
    repo.git(["add", ".gitattributes", "a.txt"]).ok();
    repo.git(["commit", "-m", "a.txt: initial commit"]).ok();
    repo
}

#[test]
fn ls_files_historical_reference_ignores_index() {
    let t = Test::new();
    let repo = txt_setup(&t, "ls-files-historical-reference-ignores-index");
    repo.git(["add", "b.txt"]).ok();
    repo.git(["commit", "-m", "b.txt: initial commit"]).ok();
    repo.git(["add", "c.txt"]).ok();

    let parent = repo.git(["rev-parse", "HEAD~1"]).ok().stdout();
    let run = repo.lfs(["ls-files", parent.trim()]).ok();
    assert_eq!(count(&run, "a.txt"), 1, "{run}");
    assert_eq!(count(&run, "b.txt"), 0, "{run}");
    assert_eq!(count(&run, "c.txt"), 0, "{run}");
}

#[test]
fn ls_files_non_head_reference_referring_to_head_ignores_index() {
    let t = Test::new();
    let repo = txt_setup(&t, "ls-files-HEAD-ish-ignores-index");
    repo.git(["tag", "v1.0.0"]).ok();
    repo.git(["add", "b.txt"]).ok();

    let run = repo.lfs(["ls-files", "v1.0.0"]).ok();
    assert_eq!(count(&run, "a.txt"), 1, "{run}");
    assert_eq!(count(&run, "b.txt"), 0, "{run}");
}

#[test]
fn ls_files_outside_git_repository() {
    let t = Test::new();
    let run = t.command(t.dir(), git_lfs()).args(["ls-files"]).run();
    assert_eq!(run.code(), Some(128), "{run}");
    assert!(run.stdout().contains("Not in a Git repository"), "{run}");
}

/// Commit `files`, each holding its name, with `patterns` tracked.
fn committed<'a>(t: &'a Test, name: &str, patterns: &[&str], files: &[&str]) -> Repo<'a> {
    let repo = t.init(name);
    repo.lfs(["track"].iter().chain(patterns)).ok();
    for file in files {
        repo.write(file, format!("{file}\n"));
    }
    repo.commit_all("initial commit");
    repo
}

#[test]
fn ls_files_include() {
    let t = Test::new();
    let repo = committed(
        &t,
        "ls-files-include",
        &["*.dat", "*.bin"],
        &["a.dat", "b.dat", "c.bin"],
    );
    let run = repo.lfs(["ls-files", "--include=*.dat"]).ok();
    assert_eq!(count(&run, ".bin"), 0, "{run}");
    assert_eq!(count(&run, ".dat"), 2, "{run}");
}

#[test]
fn ls_files_exclude() {
    let t = Test::new();
    let repo = committed(
        &t,
        "ls-files-exclude",
        &["*.dat"],
        &["a.dat", "b.dat", "dir/c.dat"],
    );
    let run = repo.lfs(["ls-files", "--exclude=dir/"]).ok();
    assert_eq!(count(&run, "dir"), 0, "{run}");
    assert_eq!(count(&run, ".dat"), 2, "{run}");
}

#[test]
fn ls_files_include_exclude_with_path_cache_settings() {
    let t = Test::new();
    let repo = committed(
        &t,
        "ls-files-include-exclude",
        &["*.dat"],
        &["a.dat", "dir/b.dat", "dir/c.dat", "dir/d.dat"],
    );
    let check = || {
        let run = repo
            .lfs(["ls-files", "--include=dir/", "--exclude=c.dat"])
            .ok();
        assert_eq!(count(&run, "dir/b.dat"), 1, "{run}");
        assert_eq!(count(&run, "dir/d.dat"), 1, "{run}");
        assert_eq!(run.output().lines().count(), 2, "{run}");
    };
    check();
    for cache in ["none", "0", "1", "unlimited", "", "-1", "invalid"] {
        repo.git(["config", "lfs.pathFilterCacheSize", cache]).ok();
        check();
    }
}

#[test]
fn ls_files_before_first_commit() {
    let t = Test::new();
    let repo = t.init("ls-files-before-first-commit");
    let run = repo.lfs(["ls-files"]);
    assert_eq!(run.stdout(), "", "{run}");
}

/// `one.tgz` and `two.tgz` with the same contents.
fn duplicates<'a>(t: &'a Test, name: &str) -> Repo<'a> {
    let repo = t.init(name);
    repo.lfs(["track", "*.tgz"]).ok().has(r#"Tracking "*.tgz""#);
    repo.write("one.tgz", "test content\n");
    repo.write("two.tgz", "test content\n");
    repo.git(["add", "one.tgz"]).ok();
    repo.git(["add", "two.tgz"]).ok();
    repo.git(["commit", "-m", "add duplicate files"]).ok();
    repo
}

#[test]
fn ls_files_show_duplicate_files() {
    let t = Test::new();
    let repo = duplicates(&t, "dupRepoShort");
    assert_output(
        &repo.lfs(["ls-files"]).ok(),
        "a1fff0ffef * one.tgz\na1fff0ffef * two.tgz",
    );
}

#[test]
fn ls_files_show_duplicate_files_with_long_oid() {
    let t = Test::new();
    let repo = duplicates(&t, "dupRepoLong");
    let oid = "a1fff0ffefb9eace7230c24e50731f0a91c62f9cefdfe77121c2f607125dffae";
    assert_output(
        &repo.lfs(["ls-files", "--long"]).ok(),
        &format!("{oid} * one.tgz\n{oid} * two.tgz"),
    );
}

/// `a.dat` and `b.dat` committed, and `removed` removed in a second
/// commit.
fn removed_in_history<'a>(t: &'a Test, name: &str, removed: &str) -> Repo<'a> {
    let repo = t.init(name);
    repo.lfs(["track", "*.dat"]).ok();
    repo.write("a.dat", "a");
    repo.write("b.dat", "b");
    repo.git(["add", ".gitattributes", "a.dat", "b.dat"]).ok();
    repo.git(["commit", "-m", "initial commit"]).ok();
    repo.git(["rm", removed]).ok();
    repo.git(["commit", "-m", &format!("remove {removed}")])
        .ok();
    repo
}

#[test]
fn ls_files_history_with_all() {
    let t = Test::new();
    let repo = removed_in_history(&t, "ls-files-history-with-all", "b.dat");
    let run = repo.lfs(["ls-files"]).ok();
    assert_eq!(count(&run, "a.dat"), 1, "{run}");
    assert_eq!(count(&run, "b.dat"), 0, "{run}");
    let run = repo.lfs(["ls-files", "--all"]).ok();
    assert_eq!(count(&run, "a.dat"), 1, "{run}");
    assert_eq!(count(&run, "b.dat"), 1, "{run}");
}

#[test]
fn ls_files_all_with_arguments() {
    let t = Test::new();
    let repo = t.init("ls-files-all-with-arguments");
    let run = repo.lfs(["ls-files", "--all", "main"]).fails();
    assert_eq!(
        run.output().trim_end(),
        "Cannot use --all with explicit reference",
        "{run}"
    );
}

#[test]
fn ls_files_reference_with_deleted() {
    let t = Test::new();
    let repo = removed_in_history(&t, "ls-files-reference-with-deleted", "a.dat");
    let run = repo.lfs(["ls-files"]).ok();
    assert_eq!(count(&run, "a.dat"), 0, "{run}");
    let run = repo.lfs(["ls-files", "--deleted"]).ok();
    assert_eq!(count(&run, "a.dat"), 1, "{run}");
}

#[test]
fn ls_files_invalid_all_ordering() {
    let t = Test::new();
    let repo = committed(
        &t,
        "ls-files-invalid---all-ordering",
        &["*.dat"],
        &["a.dat"],
    );
    repo.lfs(["ls-files", "--", "--all"])
        .fails()
        .has("Did you mean `git lfs ls-files --all --` ?");
}

#[test]
fn ls_files_list_stat_files_with_escaped_runes_in_path_before_commit() {
    let t = Test::new();
    let repo = t.init("runes-in-path");
    repo.lfs(["track", "**/*"]).ok();
    for file in [
        "regular",
        "schüüch.bin",
        "german/äöü/regular",
        "german/äöü/schüüch.bin",
    ] {
        repo.write(file, "zero\n");
    }
    // Like the shell's `git add *`, which leaves out .gitattributes.
    repo.git(["add", "regular", "schüüch.bin", "german"]).ok();

    for args in [&["ls-files"][..], &["ls-files", "-l"]] {
        let run = repo.lfs(args).ok();
        assert_eq!(count(&run, "*"), 4, "{run}");
    }
}

#[test]
fn ls_files_name_only() {
    let t = Test::new();
    let repo = tracked(&t, "ls-files-name");
    repo.write("a.dat", "test contents\n");
    repo.git(["add", "a.dat"]).ok();
    repo.git(["commit", "-m", "add a.dat"]).ok();
    let run = repo.lfs(["ls-files", "--name-only"]).ok();
    assert_eq!(run.output().trim_end(), "a.dat", "{run}");
}

#[test]
fn ls_files_history_with_reference_range() {
    let t = Test::new();
    let repo = tracked(&t, "ls-files-history-with-range");
    let commit = |file: &str, contents: &str, message: &str| {
        repo.write(file, contents);
        repo.git(["add", file]).ok();
        repo.git(["commit", "-m", message]).ok();
    };
    commit("a.dat", "content of a-file\n", "add a.dat");
    commit("b.dat", "content of b-file\n", "add b.dat");
    repo.git(["tag", "b-commit"]).ok();
    commit("c.dat", "content of c-file\n", "add c.dat");
    commit(
        "c.dat",
        "content of c-file and later modified\n",
        "modify c.dat",
    );
    repo.git(["tag", "c-commit"]).ok();
    repo.git(["rm", "a.dat"]).ok();
    repo.git(["commit", "-m", "remove a.dat"]).ok();

    // How many times each of a.dat, b.dat, and c.dat is listed.
    for (args, counts) in [
        (&["--all"][..], [1, 1, 2]),
        (&["b-commit", "c-commit"], [0, 0, 2]),
        (&["c-commit~", "c-commit"], [0, 0, 1]),
        (&["HEAD~", "HEAD"], [0, 0, 0]),
    ] {
        let run = repo.lfs(["ls-files"].iter().chain(args)).ok();
        for (file, n) in ["a.dat", "b.dat", "c.dat"].iter().zip(counts) {
            assert_eq!(count(&run, file), n, "{file} with {args:?}\n{run}");
        }
    }
}

#[test]
fn ls_files_not_affected_by_lfs_fetchexclude() {
    let t = Test::new();
    let repo = some_and_missing(&t, "repo-fetchexclude");
    repo.git(["config", "lfs.fetchexclude", "*"]).ok();
    assert_output(&repo.lfs(["ls-files"]).ok(), "6bbd052ab0 * missing.dat");
}
