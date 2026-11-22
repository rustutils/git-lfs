//! `git lfs completion`, and the hidden `__complete` command its scripts
//! call.
//!
//! The scripts are upstream's, which its cobra library generates. At each
//! TAB they run `git-lfs __completeNoDesc <words>...` (or `__complete`,
//! which adds descriptions) and read one candidate per line, then a
//! `:<directive>` line telling the shell what else to do. [`complete`]
//! answers from the clap command tree with cobra's rules, so scripts
//! installed from upstream work too.

use clap::{Arg, ArgAction, Command};

const BASH: &str = include_str!("../completions/git-lfs-completion.bash");
const FISH: &str = include_str!("../completions/git-lfs-completion.fish");
const ZSH: &str = include_str!("../completions/git-lfs-completion.zsh");

/// Run `git lfs completion` with its positional arguments.
///
/// Errors use cobra's wording, which the shell tests match.
pub fn run(shell: Option<&str>) -> u8 {
    let script = match shell {
        Some("bash") => BASH,
        Some("fish") => FISH,
        Some("zsh") => ZSH,
        Some(other) => {
            eprintln!("Error: invalid argument {other:?} for \"git-lfs completion\"");
            return 2;
        }
        None => {
            eprintln!("Error: accepts 1 arg(s), received 0");
            return 2;
        }
    };
    print!("{script}");
    0
}

/// Let the shell complete files when there are no candidates.
const DEFAULT: u32 = 0;
/// Don't complete files, even with no candidates.
const NO_FILE_COMP: u32 = 4;

/// Candidates for the word being completed, as `__complete` prints them.
#[derive(Debug, PartialEq)]
pub struct Completions {
    /// Each candidate, with its description if it has one.
    pub candidates: Vec<(String, Option<String>)>,
    /// Cobra's directive bits.
    pub directive: u32,
    /// Why no completion could be found, for stderr.
    pub error: Option<String>,
}

impl Completions {
    fn new(directive: u32) -> Self {
        Self {
            candidates: Vec::new(),
            directive,
            error: None,
        }
    }

    fn push(&mut self, name: String, desc: Option<String>) {
        self.candidates.push((name, desc));
    }
}

/// Run `__complete` (with descriptions) or `__completeNoDesc`.
///
/// `args` are the words after the command name. The last is the word
/// being completed, often empty.
pub fn run_complete(mut root: Command, args: &[String], descriptions: bool) -> u8 {
    root.build();
    let out = complete(&root, args);
    if let Some(e) = &out.error {
        eprintln!("[Error] {e}");
    }
    let mut stdout = String::new();
    for (name, desc) in &out.candidates {
        let line = match desc {
            Some(d) if descriptions => format!("{name}\t{d}"),
            _ => name.clone(),
        };
        // Only the first line, and no trailing whitespace, or the scripts
        // would misread it.
        let line = line.lines().next().unwrap_or("").trim();
        stdout.push_str(line);
        stdout.push('\n');
    }
    println!("{stdout}:{}", out.directive);
    eprintln!(
        "Completion ended with directive: {}",
        directive_names(out.directive)
    );
    0
}

/// Cobra's names for the bits in `directive`, as it prints them.
fn directive_names(directive: u32) -> String {
    const NAMES: [&str; 6] = [
        "ShellCompDirectiveError",
        "ShellCompDirectiveNoSpace",
        "ShellCompDirectiveNoFileComp",
        "ShellCompDirectiveFilterFileExt",
        "ShellCompDirectiveFilterDirs",
        "ShellCompDirectiveKeepOrder",
    ];
    let set: Vec<&str> = NAMES
        .iter()
        .enumerate()
        .filter(|(i, _)| directive & (1 << i) != 0)
        .map(|(_, n)| *n)
        .collect();
    if set.is_empty() {
        "ShellCompDirectiveDefault".into()
    } else {
        set.join(", ")
    }
}

/// What the command line before the completed word says.
struct Parsed<'a> {
    /// The innermost subcommand named.
    cmd: &'a Command,
    /// The subcommand names leading to `cmd`.
    path: Vec<&'a str>,
    positionals: Vec<&'a str>,
    /// Flags given, as the `Arg`s of `cmd` they matched.
    given: Vec<&'a Arg>,
    /// Whether `--` was given, after which nothing is a flag.
    dashdash: bool,
}

/// Find the candidates for the last of `args`, given the earlier ones.
///
/// `root` must be built, so it has its help flags and subcommand.
pub fn complete(root: &Command, args: &[String]) -> Completions {
    let (to_complete, words) = match args.split_last() {
        Some((last, words)) => (last.as_str(), words),
        None => ("", &[][..]),
    };
    let parsed = match parse(root, words) {
        Ok(p) => p,
        Err(e) => {
            let mut out = Completions::new(DEFAULT);
            out.error = Some(e);
            return out;
        }
    };
    let cmd = parsed.cmd;

    if !parsed.dashdash
        && let Some((name, value)) = flag_value_target(words, to_complete)
    {
        let Some(arg) = find_flag(cmd, &name) else {
            let mut out = Completions::new(DEFAULT);
            out.error = Some(format!(
                "subcommand '{}' does not support flag '{name}'",
                cmd.get_name()
            ));
            return out;
        };
        if takes_value(arg) {
            return complete_value(arg, value);
        }
        // A two-word flag that turned out to take no value: complete the
        // next word as usual.
    }

    let help_or_version = parsed.given.iter().any(|a| {
        matches!(a.get_action(), ArgAction::Help | ArgAction::HelpLong) || a.get_id() == "version"
    });
    if help_or_version {
        return Completions::new(NO_FILE_COMP);
    }

    if to_complete.starts_with('-') && !to_complete.contains('=') && !parsed.dashdash {
        return complete_flag_names(&parsed, to_complete);
    }

    complete_noun(root, &parsed, to_complete)
}

/// Walk `words`, descending into each subcommand named before the first
/// positional argument.
fn parse<'a>(root: &'a Command, words: &'a [String]) -> Result<Parsed<'a>, String> {
    let mut p = Parsed {
        cmd: root,
        path: Vec::new(),
        positionals: Vec::new(),
        given: Vec::new(),
        dashdash: false,
    };
    let mut i = 0;
    while i < words.len() {
        let w = words[i].as_str();
        i += 1;
        if p.dashdash {
            p.positionals.push(w);
        } else if w == "--" {
            p.dashdash = true;
        } else if let Some(long) = w.strip_prefix("--") {
            let (name, has_value) = match long.split_once('=') {
                Some((n, _)) => (n, true),
                None => (long, false),
            };
            if let Some(arg) = find_flag(p.cmd, name) {
                p.given.push(arg);
                if takes_value(arg) && !has_value {
                    i += 1;
                }
            }
        } else if w.len() > 1 && w.starts_with('-') {
            let cluster = &w[1..];
            for (at, c) in cluster.char_indices() {
                let Some(arg) = p.cmd.get_arguments().find(|a| a.get_short() == Some(c)) else {
                    continue;
                };
                p.given.push(arg);
                if takes_value(arg) {
                    // The value is the rest of the word, or the next one.
                    if at + c.len_utf8() == cluster.len() {
                        i += 1;
                    }
                    break;
                }
            }
        } else if p.positionals.is_empty()
            && let Some(sub) = p.cmd.find_subcommand(w)
        {
            p.cmd = sub;
            p.path.push(sub.get_name());
        } else if p.path.is_empty() && p.cmd.has_subcommands() {
            return Err(format!("unable to find a command for arguments: [{w}]"));
        } else {
            p.positionals.push(w);
        }
    }
    Ok(p)
}

/// The flag whose value is being completed, and the value typed so far.
///
/// That's `--name=<value>` or `-x=<value>` in the completed word, or a
/// flag without `=` as the previous word.
fn flag_value_target<'a>(words: &[String], to_complete: &'a str) -> Option<(String, &'a str)> {
    if to_complete.starts_with('-') {
        let (flag, value) = to_complete.split_once('=')?;
        let name = match flag.strip_prefix("--") {
            Some(long) => long.to_owned(),
            None => flag.chars().last()?.to_string(),
        };
        return Some((name, value));
    }
    let prev = words.last()?;
    let is_flag = (prev.len() >= 3 && prev.as_bytes()[1] == b'-' && prev.starts_with('-'))
        || (prev.len() >= 2 && prev.starts_with('-') && prev.as_bytes()[1] != b'-');
    if !is_flag || prev.contains('=') {
        return None;
    }
    let name = match prev.strip_prefix("--") {
        Some(long) => long.to_owned(),
        None => prev.chars().last()?.to_string(),
    };
    Some((name, to_complete))
}

/// The flag of `cmd` with long name `name`, or short name if `name` is
/// one character.
fn find_flag<'a>(cmd: &'a Command, name: &str) -> Option<&'a Arg> {
    let mut chars = name.chars();
    let short = match (chars.next(), chars.next()) {
        (Some(c), None) => Some(c),
        _ => None,
    };
    cmd.get_arguments().find(|a| {
        !a.is_positional()
            && (a.get_long() == Some(name)
                || a.get_all_aliases().is_some_and(|al| al.contains(&name))
                || (short.is_some() && a.get_short() == short))
    })
}

fn takes_value(arg: &Arg) -> bool {
    arg.get_action().takes_values()
}

/// Complete a flag's value from its possible values, or else as a file.
fn complete_value(arg: &Arg, typed: &str) -> Completions {
    let possible = arg.get_possible_values();
    if possible.is_empty() {
        return Completions::new(DEFAULT);
    }
    let mut out = Completions::new(NO_FILE_COMP);
    for v in possible.iter().filter(|v| !v.is_hide_set()) {
        if v.get_name().starts_with(typed) {
            out.push(
                v.get_name().to_owned(),
                v.get_help().map(ToString::to_string),
            );
        }
    }
    out
}

/// List the flags of the command that match `typed`, global ones first.
fn complete_flag_names(parsed: &Parsed<'_>, typed: &str) -> Completions {
    let mut flags: Vec<&Arg> = parsed
        .cmd
        .get_arguments()
        .filter(|a| !a.is_positional() && !a.is_hide_set())
        .filter(|a| {
            let repeatable = matches!(a.get_action(), ArgAction::Append | ArgAction::Count);
            repeatable || !parsed.given.iter().any(|g| g.get_id() == a.get_id())
        })
        .collect();
    let sort_name = |a: &Arg| {
        a.get_long()
            .map(str::to_owned)
            .or_else(|| a.get_short().map(String::from))
            .unwrap_or_default()
    };
    flags.sort_by_key(|a| (!a.is_global_set(), sort_name(a)));

    let mut out = Completions::new(NO_FILE_COMP);
    for arg in flags {
        let desc = arg.get_help().map(ToString::to_string);
        if let Some(long) = arg.get_long() {
            let name = format!("--{long}");
            if name.starts_with(typed) {
                out.push(name, desc.clone());
            }
        }
        if let Some(short) = arg.get_short() {
            let name = format!("-{short}");
            if name.starts_with(typed) {
                out.push(name, desc);
            }
        }
    }
    out
}

/// Complete a word that isn't a flag: a subcommand name, one of the first
/// positional argument's possible values, or else a file.
fn complete_noun(root: &Command, parsed: &Parsed<'_>, typed: &str) -> Completions {
    let mut out = Completions::new(DEFAULT);

    // `git lfs help <command>...` completes command names, which upstream
    // doesn't.
    if parsed.path == ["help"] {
        let mut cmd = root;
        for name in &parsed.positionals {
            match cmd.find_subcommand(name) {
                Some(sub) => cmd = sub,
                None => return out,
            }
        }
        out.directive = NO_FILE_COMP;
        push_subcommands(&mut out, cmd, typed, false);
        return out;
    }

    let local_flag_given = parsed.given.iter().any(|a| !a.is_global_set());
    if parsed.positionals.is_empty() && !local_flag_given && parsed.cmd.has_subcommands() {
        out.directive = NO_FILE_COMP;
        // Like cobra, only the root has a help subcommand to offer.
        push_subcommands(&mut out, parsed.cmd, typed, parsed.path.is_empty());
    }

    let first_positional = parsed.cmd.get_arguments().find(|a| a.is_positional());
    if let Some(arg) = first_positional
        && parsed.positionals.is_empty()
    {
        let possible = arg.get_possible_values();
        if !possible.is_empty() {
            out.directive = NO_FILE_COMP;
            for v in possible.iter().filter(|v| !v.is_hide_set()) {
                if v.get_name().starts_with(typed) {
                    out.push(
                        v.get_name().to_owned(),
                        v.get_help().map(ToString::to_string),
                    );
                }
            }
        }
    }
    out
}

/// Add the visible subcommands of `cmd` that start with `typed`, sorted.
fn push_subcommands(out: &mut Completions, cmd: &Command, typed: &str, with_help: bool) {
    let mut subs: Vec<&Command> = cmd
        .get_subcommands()
        .filter(|s| !s.is_hide_set())
        .filter(|s| with_help || s.get_name() != "help")
        .collect();
    subs.sort_by_key(|s| s.get_name());
    for sub in subs {
        if sub.get_name().starts_with(typed) {
            let desc = sub.get_about().map(ToString::to_string);
            out.push(sub.get_name().to_owned(), desc);
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use clap::CommandFactory;

    fn root() -> Command {
        let mut cmd = git_lfs::args::Cli::command();
        cmd.build();
        cmd
    }

    fn names(args: &[&str]) -> (Vec<String>, u32) {
        let args: Vec<String> = args.iter().map(|s| s.to_string()).collect();
        let out = complete(&root(), &args);
        (
            out.candidates.into_iter().map(|(n, _)| n).collect(),
            out.directive,
        )
    }

    #[test]
    fn completes_subcommand_names() {
        let (names, directive) = names(&["lo"]);
        assert_eq!(names, ["lock", "locks", "logs"]);
        assert_eq!(directive, NO_FILE_COMP);
    }

    #[test]
    fn lists_help_only_at_the_root() {
        assert!(names(&[""]).0.contains(&"help".to_owned()));
        let (names, _) = names(&["migrate", ""]);
        assert_eq!(names, ["export", "import", "info"]);
    }

    #[test]
    fn completes_flag_names_after_a_subcommand() {
        let (names, directive) = names(&["push", "--dry"]);
        assert_eq!(names, ["--dry-run"]);
        assert_eq!(directive, NO_FILE_COMP);
    }

    #[test]
    fn leaves_a_given_flag_out() {
        let (names, _) = names(&["push", "--dry-run", "--dry"]);
        assert!(names.is_empty());
    }

    #[test]
    fn completes_files_for_positional_arguments() {
        assert_eq!(names(&["track", ""]), (vec![], DEFAULT));
    }

    #[test]
    fn completes_files_for_a_flag_value() {
        assert_eq!(names(&["fetch", "--include", ""]), (vec![], DEFAULT));
        assert_eq!(names(&["fetch", "--include=a"]), (vec![], DEFAULT));
    }

    #[test]
    fn completes_the_shell_for_completion() {
        let (names, directive) = names(&["completion", ""]);
        assert_eq!(names, ["bash", "fish", "zsh"]);
        assert_eq!(directive, NO_FILE_COMP);
    }

    #[test]
    fn completes_command_names_after_help() {
        let (names, _) = names(&["help", "mig"]);
        assert_eq!(names, ["migrate"]);
    }

    #[test]
    fn an_unknown_command_is_an_error() {
        let args: Vec<String> = vec!["nope".into(), "".into()];
        let out = complete(&root(), &args);
        assert!(out.candidates.is_empty());
        assert_eq!(out.directive, DEFAULT);
        assert!(out.error.is_some());
    }

    #[test]
    fn directive_names_match_cobra() {
        assert_eq!(directive_names(0), "ShellCompDirectiveDefault");
        assert_eq!(
            directive_names(1 | NO_FILE_COMP),
            "ShellCompDirectiveError, ShellCompDirectiveNoFileComp"
        );
    }
}
