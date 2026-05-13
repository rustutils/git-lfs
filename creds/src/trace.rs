//! Shared `GIT_TRACE` gate for the creds crate's tracerx-style logs.

/// Mirrors git's `GIT_TRACE` semantics: any value other than `""`,
/// `0`, `false`, `no`, `off` enables tracing.
pub(crate) fn trace_enabled() -> bool {
    match std::env::var_os("GIT_TRACE") {
        None => false,
        Some(v) => {
            let s = v.to_string_lossy().trim().to_lowercase();
            !matches!(s.as_str(), "" | "0" | "false" | "no" | "off")
        }
    }
}
