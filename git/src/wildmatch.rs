//! The patterns upstream's wildmatch library rejects.
//!
//! Upstream drops a whole `.gitattributes` file when one of its patterns
//! fails to parse, such as `[[:space]]` with an unclosed character class.
//! Git itself accepts these patterns, and so does gix-glob, so this checks
//! them the way upstream's parser does.

/// The error upstream gives for `pattern`, or `None` if it parses.
pub(crate) fn pattern_error(pattern: &str) -> Option<String> {
    pattern.split('/').find_map(component_error)
}

/// The POSIX classes upstream knows, as in `[[:alpha:]]`.
const CLASSES: [&str; 12] = [
    "alnum", "alpha", "blank", "cntrl", "digit", "graph", "lower", "print", "punct", "space",
    "upper", "xdigit",
];

/// Check one path component, between slashes.
fn component_error(s: &str) -> Option<String> {
    let b = s.as_bytes();
    let mut i = 0;
    while i < b.len() {
        match b[i] {
            b'\\' if i + 1 >= b.len() => {
                return Some("wildmatch: unclosed escape sequence".into());
            }
            b'\\' => i += 2,
            b'[' => match group_len(&s[i..]) {
                Ok(len) => i += len,
                Err(e) => return Some(e),
            },
            _ => i += 1,
        }
    }
    None
}

/// The length of the bracket group at the start of `s`, through its `]`,
/// or the error upstream gives for it.
///
/// Like upstream, a `[:` inside the group looks for `:]` in the rest of
/// the component, and an unclosed group runs to the end.
fn group_len(s: &str) -> Result<usize, String> {
    let b = s.as_bytes();
    let mut i = 1;
    while i < b.len() {
        match b[i] {
            b'[' if b.get(i + 1) == Some(&b':') => {
                let Some(close) = s[i..].find(":]") else {
                    return Err("unclosed character class".into());
                };
                if close == 1 {
                    // `[:]` is a group with a literal `:`.
                    i += 2;
                    continue;
                }
                let name = s[i + 2..i + close].to_ascii_lowercase();
                if !CLASSES.contains(&name.as_str()) {
                    return Err(format!("wildmatch: unknown class: {name:?}"));
                }
                i += close + 2;
            }
            // A range needs an end. Upstream reads past the pattern here.
            b'-' if i + 1 >= b.len() => {
                let len = b.len();
                return Err(format!(
                    "runtime error: index out of range [{len}] with length {len}"
                ));
            }
            b'-' => i += 2,
            b'\\' if i + 1 >= b.len() => return Err("wildmatch: unclosed escape".into()),
            b'\\' => i += 2,
            b']' => return Ok(i + 1),
            _ => i += 1,
        }
    }
    Ok(b.len())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn valid_patterns_pass() {
        for p in [
            "*.dat",
            "file[[:space:]]name",
            "[!a-z]*.bin",
            "a[:]b",
            "dir/**/x[[:DIGIT:]]",
            "\\[literal",
            "unclosed[group",
        ] {
            assert_eq!(pattern_error(p), None, "{p}");
        }
    }

    #[test]
    fn upstream_errors_are_reported() {
        assert_eq!(
            pattern_error("file[[:space:]]with[[:space]]incomplete_class").as_deref(),
            Some("unclosed character class")
        );
        assert_eq!(
            pattern_error("file[[:space:]]with[[:invalid:]]class").as_deref(),
            Some("wildmatch: unknown class: \"invalid\"")
        );
        assert_eq!(
            pattern_error("trailing\\").as_deref(),
            Some("wildmatch: unclosed escape sequence")
        );
        assert_eq!(
            pattern_error("x[a\\").as_deref(),
            Some("wildmatch: unclosed escape")
        );
    }

    #[test]
    fn classes_do_not_reach_across_slashes() {
        assert_eq!(
            pattern_error("a[[:space/x:]").as_deref(),
            Some("unclosed character class")
        );
    }
}
