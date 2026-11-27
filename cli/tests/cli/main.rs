//! The shell suites in `tests/`, ported to Rust.
//!
//! Each module is one suite, such as `push` for `t-push.sh`, and its tests
//! keep the shell tests' names. [`harness`] has the setup they share.

mod conflicts;
mod harness;
mod migrate_fixtures;
mod transfer_fixtures;

mod alternates;
mod batch_retries_ratelimit;
mod batch_storage_encoding;
mod batch_storage_retries;
mod batch_storage_retries_ratelimit;
mod checkout;
mod clean;
mod clone;
mod completion;
mod config;
mod env;
mod fetch;
mod fetch_paths;
mod fetch_recent;
mod fetch_refspec;
mod filter_process;
mod fsck;
mod happy_path;
mod header_redirect;
mod install;
mod install_custom_hooks_path;
mod install_worktree;
mod lock;
mod locks;
mod ls_files;
mod merge_driver;
mod migrate_export;
mod migrate_fixup;
mod migrate_import;
mod migrate_import_no_rewrite;
mod migrate_info;
mod multiple_remotes;
mod pointer;
mod pre_push;
mod progress;
mod prune;
mod pull;
mod push;
mod push_failures_remote;
mod smudge;
mod status;
mod track;
mod umask;
mod uninstall;
mod uninstall_worktree;
mod unlock;
mod untrack;
mod update;
mod verify;
