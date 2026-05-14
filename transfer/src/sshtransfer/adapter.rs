//! Pure-SSH transfer adapter: batch + download.
//!
//! Sits between the transfer queue and the [`Pool`] of SSH
//! connections. Both entry points are `async` because the queue is
//! `tokio`-based; internally they `spawn_blocking` to run the
//! sync-only SSH connection I/O off the runtime thread.
//!
//! Wire format reference: `docs/proposals/ssh_adapter.md`.

use std::sync::Arc;

use git_lfs_api::{Action, Actions, BatchRequest, BatchResponse, ObjectResult, Operation};
use git_lfs_pointer::Oid;
use git_lfs_store::Store;
use tokio::sync::mpsc::UnboundedSender;

use crate::error::TransferError;
use crate::event::Event;
use crate::sshtransfer::pool::Pool;

/// Run a batch request over the SSH connection pool.
///
/// Translates `BatchRequest` into a `batch` command on the pool's
/// master connection: serializes each object as `"<oid> <size>"`
/// data lines, sends `transfer=ssh hash-algo=<algo> refname=<name>`
/// as text args, and parses the per-object response lines back
/// into a `BatchResponse`. `Action.href` is left empty (the SSH
/// transfer doesn't dial a URL); `Action.id` / `Action.token` /
/// `Action.expires_in` / `Action.expires_at` are populated from
/// the response's `id=…` / `token=…` / `expires-in=…` /
/// `expires-at=…` key-value pairs.
pub async fn batch(pool: Arc<Pool>, req: &BatchRequest) -> Result<BatchResponse, TransferError> {
    // Build inputs synchronously so the blocking task only has to
    // do the actual I/O.
    let mut args: Vec<String> = vec!["transfer=ssh".to_owned()];
    let hash = req.hash_algo.clone().unwrap_or_else(|| "sha256".to_owned());
    args.push(format!("hash-algo={hash}"));
    if let Some(r) = req.r#ref.as_ref() {
        args.push(format!("refname={}", r.name));
    }
    let lines: Vec<String> = req
        .objects
        .iter()
        .map(|o| format!("{} {}", o.oid, o.size))
        .collect();

    let operation = req.operation;

    let resp = tokio::task::spawn_blocking(move || -> Result<BatchResponse, TransferError> {
        let mut guard = pool
            .acquire()
            .map_err(|e| TransferError::Io(std::io::Error::other(e.to_string())))?;
        let conn = guard.connection();
        let arg_refs: Vec<&str> = args.iter().map(String::as_str).collect();
        let line_refs: Vec<&str> = lines.iter().map(String::as_str).collect();
        conn.stream()
            .send_command_with_lines("batch", &arg_refs, &line_refs)
            .map_err(TransferError::Io)?;
        let (status, resp_args, oid_lines) = conn
            .stream()
            .read_status_with_lines()
            .map_err(|e| TransferError::Io(std::io::Error::other(e.to_string())))?;
        if !(200..300).contains(&status) {
            let detail = oid_lines.first().cloned().unwrap_or_default();
            guard.discard();
            return Err(TransferError::Io(std::io::Error::other(format!(
                "SSH batch returned status {status}: {detail}"
            ))));
        }
        // Validate hash algo if the server advertised it.
        let mut server_hash: Option<String> = None;
        for arg in &resp_args {
            if let Some(rest) = arg.strip_prefix("hash-algo=") {
                server_hash = Some(rest.to_owned());
            }
        }
        if let Some(h) = &server_hash
            && !h.eq_ignore_ascii_case("sha256")
        {
            return Err(TransferError::UnsupportedHashAlgo(h.clone()));
        }

        let objects = parse_oid_lines(operation, oid_lines)?;
        Ok(BatchResponse {
            transfer: Some("ssh".to_owned()),
            objects,
            hash_algo: server_hash,
        })
    })
    .await
    .map_err(|e| TransferError::Io(std::io::Error::other(e.to_string())))??;

    Ok(resp)
}

/// Parse `batch-oid-line` rows into `ObjectResult`s.
///
/// Each line is `"<oid> <size> <action> [key=value]..."`. The same
/// OID may appear on multiple lines (e.g. upload + verify), so we
/// sort first then merge consecutive same-OID rows into a single
/// `ObjectResult.actions`. `noop` action means the server already
/// has the object — emitted as `ObjectResult.actions = None` to
/// match the HTTP batch convention.
fn parse_oid_lines(
    operation: Operation,
    mut lines: Vec<String>,
) -> Result<Vec<ObjectResult>, TransferError> {
    lines.sort();
    let mut out: Vec<ObjectResult> = Vec::new();
    for line in lines {
        let mut parts = line.splitn(4, ' ');
        let oid = parts.next().ok_or_else(|| {
            TransferError::Io(std::io::Error::other(format!(
                "malformed batch line: {line:?}"
            )))
        })?;
        let size_str = parts.next().ok_or_else(|| {
            TransferError::Io(std::io::Error::other(format!(
                "malformed batch line: {line:?}"
            )))
        })?;
        let size: u64 = size_str.parse().map_err(|_| {
            TransferError::Io(std::io::Error::other(format!(
                "invalid size in batch line: {line:?}"
            )))
        })?;
        let action = parts.next().ok_or_else(|| {
            TransferError::Io(std::io::Error::other(format!(
                "missing action in batch line: {line:?}"
            )))
        })?;
        let rest = parts.next().unwrap_or("");

        // Land on or create the ObjectResult for this OID.
        let needs_new = out.last().is_none_or(|o| o.oid != oid);
        if needs_new {
            out.push(ObjectResult {
                oid: oid.to_owned(),
                size,
                authenticated: None,
                actions: None,
                error: None,
            });
        }
        let target = out.last_mut().expect("just pushed");

        if action == "noop" {
            // Server already has it; nothing to do.
            continue;
        }

        let action_value = parse_action_kv(rest);
        let actions = target.actions.get_or_insert_with(Actions::default);
        match (operation, action) {
            (Operation::Download, "download") => actions.download = Some(action_value),
            (Operation::Upload, "upload") => actions.upload = Some(action_value),
            (Operation::Upload, "verify") => actions.verify = Some(action_value),
            // Mismatched action for the requested operation — skip
            // silently. Upstream tolerates this for forward
            // compatibility.
            _ => {}
        }
    }
    Ok(out)
}

/// Parse the optional `key=value [key=value ...]` tail of a batch
/// line into an `Action`. Unknown keys are ignored.
fn parse_action_kv(rest: &str) -> Action {
    let mut a = Action::default();
    for kv in rest.split_whitespace() {
        let Some((k, v)) = kv.split_once('=') else {
            continue;
        };
        match k {
            "id" => a.id = Some(v.to_owned()),
            "token" => a.token = Some(v.to_owned()),
            "expires-in" => {
                if let Ok(n) = v.parse::<i64>() {
                    a.expires_in = Some(n);
                }
            }
            "expires-at" => a.expires_at = Some(v.to_owned()),
            _ => {}
        }
    }
    a
}

/// Download one object via the pool. Streams `get-object` body
/// bytes into the store while computing the SHA-256 hash on the
/// way; commits only if the hash matches `oid`.
pub async fn download(
    pool: Arc<Pool>,
    store: Arc<Store>,
    oid: &str,
    size: u64,
    action: &Action,
    events: Option<UnboundedSender<Event>>,
) -> Result<(), TransferError> {
    let oid_owned = oid.to_owned();
    let args = ssh_args(size, action);

    let result = tokio::task::spawn_blocking(move || -> Result<(), TransferError> {
        let mut guard = pool
            .acquire()
            .map_err(|e| TransferError::Io(std::io::Error::other(e.to_string())))?;
        let conn = guard.connection();
        let arg_refs: Vec<&str> = args.iter().map(String::as_str).collect();
        let cmd = format!("get-object {oid_owned}");
        conn.stream()
            .send_command(&cmd, &arg_refs)
            .map_err(TransferError::Io)?;
        let (status, resp_args) = conn
            .stream()
            .read_status_until_delim()
            .map_err(|e| TransferError::Io(std::io::Error::other(e.to_string())))?;
        if !(200..300).contains(&status) {
            // Error path: payload is text lines, not binary.
            let lines = conn
                .stream()
                .read_text_lines_until_flush()
                .unwrap_or_default();
            let detail = lines.first().cloned().unwrap_or_default();
            guard.discard();
            return Err(TransferError::Io(std::io::Error::other(format!(
                "get-object {oid_owned} returned status {status}: {detail}"
            ))));
        }

        // The server may echo size=<n> in the args. Use it as the
        // expected total when present, falling back to the size we
        // already know from the batch request.
        let advertised_size = resp_args
            .iter()
            .find_map(|a| a.strip_prefix("size="))
            .and_then(|s| s.parse::<u64>().ok())
            .unwrap_or(size);

        // Buffer the body in memory while draining packets, then
        // feed it back to `store.insert_verified` which re-hashes
        // and rejects on OID mismatch. Memory cost is O(size) per
        // concurrent download — fine for v0, future polish should
        // stream straight to disk via the store's incomplete-path
        // helpers (see `transfer/src/basic.rs`).
        let mut sink = Vec::with_capacity(usize::try_from(advertised_size).unwrap_or(0));
        let written = conn
            .stream()
            .copy_data_until_flush(&mut sink)
            .map_err(|e| TransferError::Io(std::io::Error::other(e.to_string())))?;
        if written != advertised_size {
            return Err(TransferError::Io(std::io::Error::other(format!(
                "get-object {oid_owned}: expected {advertised_size} bytes, got {written}",
            ))));
        }

        let expected_oid = Oid::from_hex(&oid_owned)?;
        let mut cursor = std::io::Cursor::new(sink);
        store.insert_verified(expected_oid, &mut cursor)?;
        Ok(())
    })
    .await
    .map_err(|e| TransferError::Io(std::io::Error::other(e.to_string())))?;

    match (result, events) {
        (Ok(()), Some(s)) => {
            let _ = s.send(Event::Progress {
                oid: oid.to_owned(),
                bytes_done: size,
            });
            Ok(())
        }
        (Ok(()), None) => Ok(()),
        (Err(e), _) => Err(e),
    }
}

/// Upload one object via the pool. Streams the on-disk object
/// bytes through `put-object`, then issues `verify-object` to
/// confirm the server accepted them. Both commands echo back the
/// `id` / `token` opaque handles the server returned in the batch
/// response so the server can correlate the call to the granted
/// permission.
pub async fn upload(
    pool: Arc<Pool>,
    store: Arc<Store>,
    oid: &str,
    size: u64,
    actions: &Actions,
    events: Option<UnboundedSender<Event>>,
) -> Result<(), TransferError> {
    let upload_action = actions
        .upload
        .as_ref()
        .ok_or_else(|| TransferError::Io(std::io::Error::other("missing upload action")))?;
    // `verify-object` is conventionally always done in the SSH
    // protocol (mirrors upstream's `verifyUpload`), but the action
    // info comes from the batch response. Use the upload action's
    // id/token by default; if a verify action was returned with its
    // own handles, prefer those.
    let verify_action_owned = actions
        .verify
        .clone()
        .unwrap_or_else(|| upload_action.clone());

    let oid_owned = oid.to_owned();
    let put_args = ssh_args(size, upload_action);
    let verify_args = ssh_args(size, &verify_action_owned);

    let oid_parsed = Oid::from_hex(oid)?;
    let object_path = store.object_path(oid_parsed);

    let result = tokio::task::spawn_blocking(move || -> Result<(), TransferError> {
        let mut guard = pool
            .acquire()
            .map_err(|e| TransferError::Io(std::io::Error::other(e.to_string())))?;
        let conn = guard.connection();

        // Open the object file and stream it.
        let mut file = std::fs::File::open(&object_path).map_err(TransferError::Io)?;
        let put_arg_refs: Vec<&str> = put_args.iter().map(String::as_str).collect();
        let cmd = format!("put-object {oid_owned}");
        conn.stream()
            .send_command_with_data(&cmd, &put_arg_refs, &mut file)
            .map_err(TransferError::Io)?;
        let (status, _args, lines) = conn
            .stream()
            .read_status_with_lines()
            .map_err(|e| TransferError::Io(std::io::Error::other(e.to_string())))?;
        if !(200..300).contains(&status) {
            let detail = lines.first().cloned().unwrap_or_default();
            guard.discard();
            return Err(TransferError::Io(std::io::Error::other(format!(
                "put-object {oid_owned} returned status {status}: {detail}"
            ))));
        }

        // Verify step. The protocol's `verify-object` echoes back
        // the same `size=<n> id=<id> token=<token>` args; the
        // server confirms (status 200) it durably has the bytes.
        let verify_arg_refs: Vec<&str> = verify_args.iter().map(String::as_str).collect();
        let verify_cmd = format!("verify-object {oid_owned}");
        conn.stream()
            .send_command(&verify_cmd, &verify_arg_refs)
            .map_err(TransferError::Io)?;
        let (vstatus, _vargs, vlines) = conn
            .stream()
            .read_status_with_lines()
            .map_err(|e| TransferError::Io(std::io::Error::other(e.to_string())))?;
        if !(200..300).contains(&vstatus) {
            let detail = vlines.first().cloned().unwrap_or_default();
            guard.discard();
            return Err(TransferError::Io(std::io::Error::other(format!(
                "verify-object {oid_owned} returned status {vstatus}: {detail}"
            ))));
        }
        Ok(())
    })
    .await
    .map_err(|e| TransferError::Io(std::io::Error::other(e.to_string())))?;

    match (result, events) {
        (Ok(()), Some(s)) => {
            let _ = s.send(Event::Progress {
                oid: oid.to_owned(),
                bytes_done: size,
            });
            Ok(())
        }
        (Ok(()), None) => Ok(()),
        (Err(e), _) => Err(e),
    }
}

fn ssh_args(size: u64, action: &Action) -> Vec<String> {
    let mut args = Vec::with_capacity(3);
    args.push(format!("size={size}"));
    if let Some(id) = action.id.as_ref() {
        args.push(format!("id={id}"));
    }
    if let Some(token) = action.token.as_ref() {
        args.push(format!("token={token}"));
    }
    args
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parse_oid_lines_groups_same_oid() {
        // Upload + verify for the same OID = one ObjectResult with
        // both upload and verify actions.
        let lines = vec![
            "abc 100 upload id=u1 token=t1".to_owned(),
            "abc 100 verify id=v1 token=t2".to_owned(),
            "def 200 noop".to_owned(),
        ];
        let out = parse_oid_lines(Operation::Upload, lines).unwrap();
        assert_eq!(out.len(), 2);

        let abc = out.iter().find(|o| o.oid == "abc").unwrap();
        assert_eq!(abc.size, 100);
        let actions = abc.actions.as_ref().unwrap();
        assert_eq!(actions.upload.as_ref().unwrap().id.as_deref(), Some("u1"));
        assert_eq!(
            actions.upload.as_ref().unwrap().token.as_deref(),
            Some("t1")
        );
        assert_eq!(actions.verify.as_ref().unwrap().id.as_deref(), Some("v1"));

        let def = out.iter().find(|o| o.oid == "def").unwrap();
        // noop ⇒ no actions populated, mirroring HTTP batch convention
        // for "server already has it".
        assert!(def.actions.is_none());
    }

    #[test]
    fn parse_oid_lines_download_action() {
        let lines = vec!["abc 100 download id=d1 token=tok expires-in=60".to_owned()];
        let out = parse_oid_lines(Operation::Download, lines).unwrap();
        assert_eq!(out.len(), 1);
        let d = out[0].actions.as_ref().unwrap().download.as_ref().unwrap();
        assert_eq!(d.id.as_deref(), Some("d1"));
        assert_eq!(d.token.as_deref(), Some("tok"));
        assert_eq!(d.expires_in, Some(60));
    }

    #[test]
    fn parse_oid_lines_drops_mismatched_action_for_operation() {
        // Download batch shouldn't end up with upload actions even
        // if the server sent them — we skip silently.
        let lines = vec!["abc 100 upload id=u1".to_owned()];
        let out = parse_oid_lines(Operation::Download, lines).unwrap();
        assert_eq!(out.len(), 1);
        // The line still created an ObjectResult, but actions stay
        // empty (no download set, the upload was ignored).
        assert!(out[0].actions.is_none() || out[0].actions.as_ref().unwrap().download.is_none());
    }

    #[test]
    fn parse_oid_lines_rejects_malformed_size() {
        let lines = vec!["abc oops download".to_owned()];
        assert!(parse_oid_lines(Operation::Download, lines).is_err());
    }

    #[test]
    fn ssh_args_includes_id_and_token_when_set() {
        let action = Action {
            id: Some("i".into()),
            token: Some("t".into()),
            ..Default::default()
        };
        let args = ssh_args(123, &action);
        assert!(args.iter().any(|a| a == "size=123"));
        assert!(args.iter().any(|a| a == "id=i"));
        assert!(args.iter().any(|a| a == "token=t"));
    }

    #[test]
    fn ssh_args_omits_id_token_when_none() {
        let action = Action::default();
        let args = ssh_args(456, &action);
        assert_eq!(args, vec!["size=456".to_owned()]);
    }
}
