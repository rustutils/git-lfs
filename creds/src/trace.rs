//! Whether to print trace lines.

/// Whether `GIT_TRACE` is set to enable tracing.
///
/// Any value enables it except empty, `0`, `false`, `no`, or `off`,
/// ignoring case and surrounding whitespace.
pub(crate) fn trace_enabled() -> bool {
    match std::env::var_os("GIT_TRACE") {
        None => false,
        Some(v) => {
            let s = v.to_string_lossy().trim().to_lowercase();
            !matches!(s.as_str(), "" | "0" | "false" | "no" | "off")
        }
    }
}
