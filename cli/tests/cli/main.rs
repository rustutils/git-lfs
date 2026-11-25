//! The shell suites in `tests/`, ported to Rust.
//!
//! Each module is one suite, such as `push` for `t-push.sh`, and its tests
//! keep the shell tests' names. [`harness`] has the setup they share.

mod conflicts;
mod harness;

mod checkout;
mod clone;
mod fetch;
mod lock;
mod locks;
mod ls_files;
mod pre_push;
mod prune;
mod pull;
mod push;
mod track;
mod unlock;
