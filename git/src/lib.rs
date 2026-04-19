//! Git interop for git-lfs: config, attributes, refs, scanners, and the
//! filter-process packet-line protocol.
//!
//! All git operations shell out to the `git` binary — see CLAUDE.md for the
//! rationale.
