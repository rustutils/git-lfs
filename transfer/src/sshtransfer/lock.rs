//! Pure-SSH lock protocol: `lock`, `unlock`, `list-lock`.
//!
//! Sync command helpers that operate on a borrowed
//! [`Connection`]. The transfer queue's batch / get-object /
//! put-object commands live in [`super::adapter`]; locking is
//! orthogonal (no transfer queue, no `spawn_blocking`) so it lives
//! here as plain sync calls — callers wrap them in
//! [`tokio::task::spawn_blocking`] if they need to drive locking from
//! an async context.
//!
//! Wire format reference:
//! [`docs/proposals/ssh_adapter.md`](https://gitlab.com/rustutils/git-lfs/-/blob/master/docs/proposals/ssh_adapter.md)
//! "Locks" section.

use std::collections::HashMap;

use crate::sshtransfer::connection::{Connection, ConnectionError};

/// One lock entry. Fields mirror the four `key=value` arguments
/// servers emit for `lock` / `unlock` responses and the four
/// `<key> <id> <value>` lock-spec lines emitted by `list-lock`.
#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub struct LockSpec {
    /// Server-assigned lock identifier.
    pub id: String,
    /// Repo-relative path that's locked.
    pub path: String,
    /// RFC 3339 timestamp when the lock was created.
    pub locked_at: String,
    /// Lock owner's display name. Absent when the server doesn't
    /// share owner info (e.g. download-mode listings).
    pub ownername: Option<String>,
}

/// Ownership annotation returned by upload-mode `list-lock`. Used by
/// `/locks/verify` parity to partition the response into ours vs.
/// theirs.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Owner {
    /// Held by the authenticated user.
    Ours,
    /// Held by someone else.
    Theirs,
    /// Server didn't supply an `owner <id> ...` line (e.g.
    /// download-mode listings).
    Unknown,
}

/// Options for [`lock`]. `refname` is the full git ref the lock
/// scopes to (`refs/heads/main`); the server's contract allows
/// it to be missing, but scutiger and upstream gitlab both require
/// it for create.
#[derive(Debug, Default, Clone)]
pub struct LockOptions {
    /// Git refname scope (`refs/heads/...`). Always emitted when
    /// `Some`; servers that ignore it tolerate the extra arg.
    pub refname: Option<String>,
}

/// Options for [`unlock`].
#[derive(Debug, Default, Clone)]
pub struct UnlockOptions {
    /// Same as [`LockOptions::refname`].
    pub refname: Option<String>,
}

/// Filter for [`list`]. Mirrors `ListLocksFilter` on the HTTP side
/// plus a `refname` rather than `refspec` (the wire arg differs).
#[derive(Debug, Default, Clone)]
pub struct ListFilter {
    /// Match a specific repo-relative path.
    pub path: Option<String>,
    /// Match a specific lock id.
    pub id: Option<String>,
    /// Pagination cursor returned by a prior listing's `next-cursor`.
    pub cursor: Option<String>,
    /// Maximum locks per response.
    pub limit: Option<u32>,
    /// Refname scope.
    pub refname: Option<String>,
}

/// Result of a `lock` or `unlock` request.
///
/// On 2xx / 409 the server returns the lock's attributes inline as
/// command arguments; we parse them into `lock`. On other statuses
/// the body carries a human-readable message in the first text line;
/// that lands in `message`.
#[derive(Debug, Clone)]
pub struct LockResponse {
    /// Raw HTTP-style status code from the `status` line.
    pub status: u16,
    /// Parsed lock attributes when present.
    pub lock: Option<LockSpec>,
    /// Server-supplied error message for non-success statuses.
    pub message: Option<String>,
}

/// Result of a `list-lock` request.
#[derive(Debug, Clone, Default)]
pub struct ListResponse {
    /// Raw status code from the `status` line.
    pub status: u16,
    /// Locks in the response, in the order the server returned them,
    /// paired with their owner annotation (`Unknown` for
    /// download-mode listings).
    pub locks: Vec<(LockSpec, Owner)>,
    /// Pagination continuation cursor when present.
    pub next_cursor: Option<String>,
    /// Server-supplied error message for non-success statuses.
    pub message: Option<String>,
}

/// Send the `lock` command and parse the response.
///
/// Wire shape: `PKT-LINE("lock")` + `PKT-LINE("path=<path>")` +
/// optional `PKT-LINE("refname=<refname>")` + flush. Response:
/// status line + zero or more arg lines + optional delim + zero or
/// more text lines + flush.
pub fn lock(
    conn: &mut Connection,
    path: &str,
    opts: &LockOptions,
) -> Result<LockResponse, ConnectionError> {
    let mut args = Vec::with_capacity(2);
    args.push(format!("path={path}"));
    if let Some(refname) = &opts.refname {
        args.push(format!("refname={refname}"));
    }
    send_and_parse_single(conn, "lock", &args)
}

/// Send the `unlock <id>` command and parse the response.
///
/// Wire shape: `PKT-LINE("unlock <id>")` + optional
/// `PKT-LINE("refname=<refname>")` + flush. The spec mentions a
/// `force` argument but neither scutiger nor upstream's
/// `sshLockClient.Unlock` emit it — force semantics live entirely on
/// the server side via auth.
pub fn unlock(
    conn: &mut Connection,
    id: &str,
    opts: &UnlockOptions,
) -> Result<LockResponse, ConnectionError> {
    let mut args = Vec::with_capacity(1);
    if let Some(refname) = &opts.refname {
        args.push(format!("refname={refname}"));
    }
    let cmd = format!("unlock {id}");
    send_and_parse_single(conn, &cmd, &args)
}

/// Send the `list-lock` command and parse the response.
///
/// In upload-mode connections, the server adds `owner <id> ours`
/// or `owner <id> theirs` lines so the caller can partition the
/// result (used by `/locks/verify` parity).
pub fn list(conn: &mut Connection, filter: &ListFilter) -> Result<ListResponse, ConnectionError> {
    let mut args = Vec::new();
    if let Some(p) = &filter.path {
        args.push(format!("path={p}"));
    }
    if let Some(i) = &filter.id {
        args.push(format!("id={i}"));
    }
    if let Some(c) = &filter.cursor {
        args.push(format!("cursor={c}"));
    }
    if let Some(l) = filter.limit {
        args.push(format!("limit={l}"));
    }
    if let Some(r) = &filter.refname {
        args.push(format!("refname={r}"));
    }
    let arg_refs: Vec<&str> = args.iter().map(String::as_str).collect();
    conn.stream().send_command("list-lock", &arg_refs)?;
    let (status, resp_args, lines) = conn.stream().read_status_with_lines()?;
    Ok(parse_list_response(status, resp_args, lines))
}

fn send_and_parse_single(
    conn: &mut Connection,
    cmd: &str,
    args: &[String],
) -> Result<LockResponse, ConnectionError> {
    let arg_refs: Vec<&str> = args.iter().map(String::as_str).collect();
    conn.stream().send_command(cmd, &arg_refs)?;
    let (status, resp_args, lines) = conn.stream().read_status_with_lines()?;
    Ok(parse_single_response(status, resp_args, lines))
}

fn parse_single_response(status: u16, args: Vec<String>, lines: Vec<String>) -> LockResponse {
    let is_success = (200..300).contains(&status);
    let is_conflict = status == 409;
    if is_success || is_conflict {
        let mut spec = LockSpec::default();
        let mut seen = 0u8;
        for arg in &args {
            if let Some(v) = arg.strip_prefix("id=") {
                spec.id = v.to_owned();
                seen |= 0b0001;
            } else if let Some(v) = arg.strip_prefix("path=") {
                spec.path = v.to_owned();
                seen |= 0b0010;
            } else if let Some(v) = arg.strip_prefix("locked-at=") {
                spec.locked_at = v.to_owned();
                seen |= 0b0100;
            } else if let Some(v) = arg.strip_prefix("ownername=") {
                spec.ownername = Some(v.to_owned());
                seen |= 0b1000;
            }
        }
        // Require all four fields to consider this a parseable
        // lock; matches upstream's `len(seen) != 4` guard.
        let lock = if seen == 0b1111 { Some(spec) } else { None };
        let message = if !is_success {
            lines.first().cloned().or_else(|| Some("conflict".into()))
        } else {
            None
        };
        LockResponse {
            status,
            lock,
            message,
        }
    } else {
        LockResponse {
            status,
            lock: None,
            message: lines.first().cloned(),
        }
    }
}

fn parse_list_response(status: u16, args: Vec<String>, lines: Vec<String>) -> ListResponse {
    if !(200..300).contains(&status) {
        return ListResponse {
            status,
            locks: Vec::new(),
            next_cursor: None,
            message: lines.first().cloned(),
        };
    }
    let next_cursor = args
        .iter()
        .find_map(|a| a.strip_prefix("next-cursor=").map(str::to_owned));

    // First pass: build per-id entries from the lock-spec lines.
    // Server is required to repeat the lock-id on every per-attribute
    // line, but it isn't required to emit them in `lock` declaration
    // order — collect by id into a map, track first-seen order
    // separately to keep the output stable.
    let mut by_id: HashMap<String, (LockSpec, Owner)> = HashMap::new();
    let mut order: Vec<String> = Vec::new();

    for line in &lines {
        let parts: Vec<&str> = line.splitn(3, ' ').collect();
        match parts.as_slice() {
            ["lock", id] if !by_id.contains_key(*id) => {
                by_id.insert(
                    (*id).to_owned(),
                    (
                        LockSpec {
                            id: (*id).to_owned(),
                            ..Default::default()
                        },
                        Owner::Unknown,
                    ),
                );
                order.push((*id).to_owned());
            }
            ["path", id, path] => {
                if let Some((s, _)) = by_id.get_mut(*id) {
                    s.path = (*path).to_owned();
                }
            }
            ["locked-at", id, ts] => {
                if let Some((s, _)) = by_id.get_mut(*id) {
                    s.locked_at = (*ts).to_owned();
                }
            }
            ["ownername", id, name] => {
                if let Some((s, _)) = by_id.get_mut(*id) {
                    s.ownername = Some((*name).to_owned());
                }
            }
            ["owner", id, who] => {
                if let Some((_, o)) = by_id.get_mut(*id) {
                    *o = match *who {
                        "ours" => Owner::Ours,
                        "theirs" => Owner::Theirs,
                        _ => Owner::Unknown,
                    };
                }
            }
            _ => {}
        }
    }

    let locks = order
        .into_iter()
        .filter_map(|id| by_id.remove(&id))
        .collect();
    ListResponse {
        status,
        locks,
        next_cursor,
        message: None,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parse_single_success_extracts_all_fields() {
        let resp = parse_single_response(
            201,
            vec![
                "id=abc".into(),
                "path=foo.bin".into(),
                "locked-at=2026-01-01T00:00:00Z".into(),
                "ownername=alice".into(),
            ],
            Vec::new(),
        );
        assert_eq!(resp.status, 201);
        let lock = resp.lock.expect("lock present");
        assert_eq!(lock.id, "abc");
        assert_eq!(lock.path, "foo.bin");
        assert_eq!(lock.locked_at, "2026-01-01T00:00:00Z");
        assert_eq!(lock.ownername.as_deref(), Some("alice"));
        assert!(resp.message.is_none());
    }

    #[test]
    fn parse_single_409_carries_conflicting_lock_and_message() {
        // 409 conflict: server returns the existing lock's args plus
        // a "conflict" text line in the post-delim section.
        let resp = parse_single_response(
            409,
            vec![
                "id=abc".into(),
                "path=foo.bin".into(),
                "locked-at=2026-01-01T00:00:00Z".into(),
                "ownername=bob".into(),
            ],
            vec!["already locked".into()],
        );
        assert_eq!(resp.status, 409);
        assert_eq!(
            resp.lock.as_ref().unwrap().ownername.as_deref(),
            Some("bob")
        );
        assert_eq!(resp.message.as_deref(), Some("already locked"));
    }

    #[test]
    fn parse_single_403_no_lock_just_message() {
        // 4xx other than 409: no lock data, just the message.
        let resp = parse_single_response(403, Vec::new(), vec!["forbidden".into()]);
        assert_eq!(resp.status, 403);
        assert!(resp.lock.is_none());
        assert_eq!(resp.message.as_deref(), Some("forbidden"));
    }

    #[test]
    fn parse_single_incomplete_fields_returns_no_lock() {
        // Missing `ownername`: upstream's "len(seen) != 4" guard
        // says we shouldn't claim a parseable lock back.
        let resp = parse_single_response(
            201,
            vec![
                "id=abc".into(),
                "path=foo.bin".into(),
                "locked-at=2026-01-01T00:00:00Z".into(),
            ],
            Vec::new(),
        );
        assert!(resp.lock.is_none());
    }

    #[test]
    fn parse_list_groups_lines_by_id_and_keeps_order() {
        let lines = vec![
            "lock id1".into(),
            "path id1 a.bin".into(),
            "locked-at id1 2026-01-01T00:00:00Z".into(),
            "ownername id1 alice".into(),
            "lock id2".into(),
            "path id2 b.bin".into(),
            "locked-at id2 2026-01-02T00:00:00Z".into(),
            "ownername id2 bob".into(),
        ];
        let resp = parse_list_response(200, Vec::new(), lines);
        assert_eq!(resp.status, 200);
        assert_eq!(resp.locks.len(), 2);
        assert_eq!(resp.locks[0].0.id, "id1");
        assert_eq!(resp.locks[0].0.path, "a.bin");
        assert_eq!(resp.locks[0].0.ownername.as_deref(), Some("alice"));
        assert_eq!(resp.locks[0].1, Owner::Unknown);
        assert_eq!(resp.locks[1].0.id, "id2");
        assert_eq!(resp.locks[1].0.path, "b.bin");
    }

    #[test]
    fn parse_list_records_owner_annotations() {
        // Upload-mode listings include `owner <id> ours|theirs`.
        let lines = vec![
            "lock id1".into(),
            "path id1 a.bin".into(),
            "locked-at id1 2026-01-01T00:00:00Z".into(),
            "ownername id1 alice".into(),
            "owner id1 ours".into(),
            "lock id2".into(),
            "path id2 b.bin".into(),
            "locked-at id2 2026-01-02T00:00:00Z".into(),
            "ownername id2 bob".into(),
            "owner id2 theirs".into(),
        ];
        let resp = parse_list_response(200, Vec::new(), lines);
        assert_eq!(resp.locks[0].1, Owner::Ours);
        assert_eq!(resp.locks[1].1, Owner::Theirs);
    }

    #[test]
    fn parse_list_picks_up_next_cursor() {
        let resp = parse_list_response(200, vec!["next-cursor=abc123".into()], Vec::new());
        assert_eq!(resp.next_cursor.as_deref(), Some("abc123"));
    }

    #[test]
    fn parse_list_error_carries_message() {
        let resp = parse_list_response(404, Vec::new(), vec!["not found".into()]);
        assert_eq!(resp.status, 404);
        assert!(resp.locks.is_empty());
        assert_eq!(resp.message.as_deref(), Some("not found"));
    }

    #[test]
    fn parse_list_tolerates_interspersed_lines() {
        // Server emits attributes for id2 between id1's lines.
        // Map-based grouping should still produce the right result.
        let lines = vec![
            "lock id1".into(),
            "lock id2".into(),
            "path id1 a.bin".into(),
            "path id2 b.bin".into(),
            "locked-at id1 2026-01-01T00:00:00Z".into(),
            "locked-at id2 2026-01-02T00:00:00Z".into(),
            "ownername id1 alice".into(),
            "ownername id2 bob".into(),
        ];
        let resp = parse_list_response(200, Vec::new(), lines);
        assert_eq!(resp.locks.len(), 2);
        assert_eq!(resp.locks[0].0.path, "a.bin");
        assert_eq!(resp.locks[1].0.path, "b.bin");
    }

    #[test]
    fn parse_list_ignores_lines_for_unknown_id() {
        // No `lock id1` declarator — the per-attribute lines for id1
        // get dropped on the floor rather than spawning a stub entry.
        let lines = vec![
            "path id1 a.bin".into(),
            "lock id2".into(),
            "path id2 b.bin".into(),
            "locked-at id2 2026-01-02T00:00:00Z".into(),
            "ownername id2 bob".into(),
        ];
        let resp = parse_list_response(200, Vec::new(), lines);
        assert_eq!(resp.locks.len(), 1);
        assert_eq!(resp.locks[0].0.id, "id2");
    }
}
