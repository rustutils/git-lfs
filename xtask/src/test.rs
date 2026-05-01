//! Run the upstream shell test suite via `make test`, parse the streamed
//! TAP output, and print a clean per-suite summary at the end.
//!
//! The Makefile already owns the build / setup / shutdown plumbing, so
//! this is a pure pass-through plus a parser. Each shell suite emits
//! standard TAP (`ok N - desc`, `not ok N - desc`) inside its own
//! `t-*.sh` block under `prove -v`, and the summary groups results by
//! suite for at-a-glance triage.

use std::collections::BTreeMap;
use std::io::{BufRead, BufReader, Write};
use std::path::Path;
use std::process::{Command, Stdio};

#[derive(Default)]
struct Suite {
    pass: usize,
    total: usize,
    failures: Vec<String>,
}

/// Run upstream shell tests via `make` in `tests_dir`, stream the
/// output, and print a per-suite summary at the end.
///
/// With no suite names: invokes `make test PROVE_EXTRA_ARGS=-v`,
/// which runs every `t-*.sh` under one outer setup/shutdown of
/// `lfstest-gitserver`. With one or more suite names: invokes
/// `make t-<name>.sh ...`, where each per-suite goal already enables
/// `-v` in the Makefile recipe. Names may be passed with or without
/// the `t-` prefix and `.sh` suffix.
pub fn run(tests_dir: &Path, suites: &[String], show_failures: bool) -> std::io::Result<i32> {
    let mut cmd = Command::new("make");
    cmd.current_dir(tests_dir)
        .stdout(Stdio::piped())
        .stderr(Stdio::inherit());

    if suites.is_empty() {
        cmd.args(["test", "PROVE_EXTRA_ARGS=-v"]);
    } else {
        for name in suites {
            cmd.arg(normalize_suite(name));
        }
    }

    let mut child = cmd.spawn()?;

    let stdout = child.stdout.take().expect("piped");
    let reader = BufReader::new(stdout);

    let mut suites: BTreeMap<String, Suite> = BTreeMap::new();
    let mut current: Option<String> = None;
    let mut in_summary = false;

    let stdout_h = std::io::stdout();
    for line in reader.lines() {
        let line = line?;
        {
            let mut h = stdout_h.lock();
            writeln!(h, "{line}")?;
            h.flush()?;
        }

        // Once prove emits its own summary block, the lines that look
        // like suite headers (`t-foo.sh (Wstat: …)`) are no longer
        // describing live test output — skip them so we don't clobber
        // the per-suite counters we already accumulated.
        if line.starts_with("Test Summary Report") {
            in_summary = true;
            continue;
        }
        if in_summary {
            continue;
        }

        if let Some(name) = parse_suite_header(&line) {
            current = Some(name.clone());
            suites.entry(name).or_default();
            continue;
        }

        if let Some(suite) = current.as_ref().and_then(|n| suites.get_mut(n))
            && let Some(rec) = parse_tap_result(&line)
        {
            suite.total += 1;
            if rec.ok {
                suite.pass += 1;
            } else {
                suite
                    .failures
                    .push(format!("{:>3} — {}", rec.num, rec.desc));
            }
        }
    }

    let status = child.wait()?;
    print_summary(&suites, show_failures);
    Ok(status.code().unwrap_or(1))
}

/// Accept `t-foo`, `t-foo.sh`, or `./t-foo.sh`, return `t-foo.sh`.
/// Names without a `t-` prefix get one prepended so users can write
/// `cargo xtask test pull push`.
fn normalize_suite(name: &str) -> String {
    let s = name.trim().trim_start_matches("./");
    let with_prefix = if s.starts_with("t-") {
        s.to_string()
    } else {
        format!("t-{s}")
    };
    if with_prefix.ends_with(".sh") {
        with_prefix
    } else {
        format!("{with_prefix}.sh")
    }
}

/// Match prove's per-file header: `t-<name>.sh   .....   [maybe more]`.
fn parse_suite_header(line: &str) -> Option<String> {
    let trimmed = line.trim_start();
    if !trimmed.starts_with("t-") {
        return None;
    }
    let dot_sh = trimmed.find(".sh")?;
    let name = &trimmed[..dot_sh + 3];
    if name.contains(' ') {
        return None;
    }
    let after = trimmed[dot_sh + 3..].trim_start();
    if !after.starts_with('.') {
        return None;
    }
    Some(name.to_string())
}

struct TapRecord {
    ok: bool,
    num: usize,
    desc: String,
}

fn parse_tap_result(line: &str) -> Option<TapRecord> {
    let trimmed = line.trim_start();
    let (ok, rest) = if let Some(r) = trimmed.strip_prefix("ok ") {
        (true, r)
    } else if let Some(r) = trimmed.strip_prefix("not ok ") {
        (false, r)
    } else {
        return None;
    };
    let (num_part, desc_part) = rest.split_once(" - ")?;
    let num: usize = num_part.trim().parse().ok()?;
    let desc = desc_part
        .trim_end_matches(|c: char| c == '.' || c.is_whitespace())
        .to_string();
    Some(TapRecord { ok, num, desc })
}

fn print_summary(suites: &BTreeMap<String, Suite>, show_failures: bool) {
    let total_pass: usize = suites.values().map(|s| s.pass).sum();
    let total_tests: usize = suites.values().map(|s| s.total).sum();
    let total_suites = suites.len();
    let full = suites
        .values()
        .filter(|s| s.total > 0 && s.pass == s.total)
        .count();
    let partial = suites
        .values()
        .filter(|s| s.total > 0 && s.pass < s.total)
        .count();
    let empty = suites.values().filter(|s| s.total == 0).count();
    let pct = if total_tests == 0 {
        0.0
    } else {
        (total_pass as f64 / total_tests as f64) * 100.0
    };

    let max_name = suites.keys().map(|k| k.len()).max().unwrap_or(0);

    println!();
    println!("=== Test summary ===");
    println!();

    let mut any_fail = false;
    for (name, s) in suites {
        if s.total > 0 && s.pass < s.total {
            if !any_fail {
                println!("Failing suites:");
                any_fail = true;
            }
            println!(
                "  {name:<width$}  {pass}/{total}",
                width = max_name,
                pass = s.pass,
                total = s.total,
            );
            if show_failures {
                for f in &s.failures {
                    println!("      ✗ {f}");
                }
            }
        }
    }
    if any_fail {
        println!();
    }

    let mut any_pass = false;
    for (name, s) in suites {
        if s.total > 0 && s.pass == s.total {
            if !any_pass {
                println!("Passing suites:");
                any_pass = true;
            }
            println!(
                "  {name:<width$}  {pass}/{total}",
                width = max_name,
                pass = s.pass,
                total = s.total,
            );
        }
    }
    if any_pass {
        println!();
    }

    if empty > 0 {
        println!("Suites with no parsed tests:");
        for (name, s) in suites {
            if s.total == 0 {
                println!("  {name}");
            }
        }
        println!();
    }

    println!("Total:  {total_pass}/{total_tests} ({pct:.1}%)");
    println!("Suites: {total_suites} ({full} full · {partial} partial · {empty} empty)");
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn header_matches_dotted_form() {
        assert_eq!(
            parse_suite_header("t-pull.sh ............. "),
            Some("t-pull.sh".to_string())
        );
        assert_eq!(
            parse_suite_header("t-pull.sh .. 1/20"),
            Some("t-pull.sh".to_string())
        );
    }

    #[test]
    fn header_rejects_non_suite_lines() {
        assert_eq!(parse_suite_header("ok 1 - thing"), None);
        assert_eq!(parse_suite_header("Files=104, Tests=794"), None);
        assert_eq!(parse_suite_header("not ok 3 - t-pull.sh broken"), None);
        assert_eq!(parse_suite_header("t-pull.sh (Wstat: 0 ...)"), None);
    }

    #[test]
    fn tap_ok() {
        let r = parse_tap_result("ok 7 - lock a path ...                          ").unwrap();
        assert!(r.ok);
        assert_eq!(r.num, 7);
        assert_eq!(r.desc, "lock a path");
    }

    #[test]
    fn tap_not_ok() {
        let r = parse_tap_result("not ok 12 - migrate import --fixup").unwrap();
        assert!(!r.ok);
        assert_eq!(r.num, 12);
        assert_eq!(r.desc, "migrate import --fixup");
    }

    #[test]
    fn tap_rejects_garbage() {
        assert!(parse_tap_result("All tests successful.").is_none());
        assert!(parse_tap_result("# comment").is_none());
        assert!(parse_tap_result("1..3").is_none());
    }

    #[test]
    fn normalize_accepts_friendly_forms() {
        assert_eq!(normalize_suite("pull"), "t-pull.sh");
        assert_eq!(normalize_suite("t-pull"), "t-pull.sh");
        assert_eq!(normalize_suite("t-pull.sh"), "t-pull.sh");
        assert_eq!(normalize_suite("./t-pull.sh"), "t-pull.sh");
        assert_eq!(normalize_suite("  t-pull.sh  "), "t-pull.sh");
    }
}
