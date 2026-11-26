//! The shell suites in `tests/`, ported to Rust.
//!
//! Each module is one suite, such as `push` for `t-push.sh`, and its tests
//! keep the shell tests' names. [`harness`] has the setup they share.

mod conflicts;
mod harness;
mod migrate_fixtures;

mod checkout;
mod clone;
mod fetch;
mod fsck;
mod lock;
mod locks;
mod ls_files;
mod migrate_fixup;
mod migrate_import;
mod migrate_import_no_rewrite;
mod migrate_info;
mod pointer;
mod pre_push;
mod prune;
mod pull;
mod push;
mod status;
mod track;
mod unlock;
