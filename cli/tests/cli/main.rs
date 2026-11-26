//! The shell suites in `tests/`, ported to Rust.
//!
//! Each module is one suite, such as `push` for `t-push.sh`, and its tests
//! keep the shell tests' names. [`harness`] has the setup they share.

mod conflicts;
mod harness;
mod migrate_fixtures;

mod checkout;
mod clean;
mod clone;
mod config;
mod fetch;
mod filter_process;
mod fsck;
mod install;
mod install_custom_hooks_path;
mod install_worktree;
mod lock;
mod locks;
mod ls_files;
mod migrate_export;
mod migrate_fixup;
mod migrate_import;
mod migrate_import_no_rewrite;
mod migrate_info;
mod pointer;
mod pre_push;
mod prune;
mod pull;
mod push;
mod smudge;
mod status;
mod track;
mod uninstall;
mod uninstall_worktree;
mod unlock;
