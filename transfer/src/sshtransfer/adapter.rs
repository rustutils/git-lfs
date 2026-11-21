//! Batch requests, downloads, and uploads over a [`Pool`].
//!
//! Each function checks out one connection and runs the blocking protocol
//! exchange in `tokio::task::spawn_blocking`. A response with a non-2xx
//! status discards the connection.

use std::sync::Arc;

use git_lfs_api::{Action, Actions, BatchRequest, BatchResponse, ObjectResult, Operation};
use git_lfs_pointer::Oid;
use git_lfs_store::Store;
use tokio::sync::mpsc::UnboundedSender;

use crate::error::TransferError;
use crate::event::Event;
use crate::sshtransfer::pool::Pool;

/// Send a batch request as a `batch` command.
///
/// Sends the arguments `transfer=ssh`, `hash-algo` (`sha256` unless the
/// request names one), and `refname` if the request has a ref, then one
/// `<oid> <size>` line per object. The actions in the response have no
/// `href`. Their `id`, `token`, `expires_in`, and `expires_at` come from
/// the response lines.
///
/// Errors with [`TransferError::UnsupportedHashAlgo`] if the server
/// answers with a `hash-algo` other than `sha256`.
pub async fn batch(pool: Arc<Pool>, req: &BatchRequest) -> Result<BatchResponse, TransferError> {
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

/// Parse the lines of a `batch` response into one result per object.
///
/// Each line is `<oid> <size> <action> [key=value...]`. An object appears
/// once per action, for example `upload` and `verify`, so the lines are
/// sorted and merged by OID. The results come out in OID order. An object
/// listed only with `noop` gets no actions, as in the HTTP API. Actions
/// that don't belong to `operation` are ignored.
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
            continue;
        }

        let action_value = parse_action_kv(rest);
        let actions = target.actions.get_or_insert_with(Actions::default);
        match (operation, action) {
            (Operation::Download, "download") => actions.download = Some(action_value),
            (Operation::Upload, "upload") => actions.upload = Some(action_value),
            (Operation::Upload, "verify") => actions.verify = Some(action_value),
            _ => {}
        }
    }
    Ok(out)
}

/// Parse the `key=value` pairs that end a batch response line.
///
/// Unknown keys, and an `expires-in` that isn't an integer, are ignored.
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

/// Download one object with `get-object` into the store.
///
/// The body must have the size the server states in its `size`
/// argument, or `size` if it states none. The object is stored only if
/// its hash matches `oid`. The whole body is held in memory first.
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
            // After an error status, the data is a text message.
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

        let advertised_size = resp_args
            .iter()
            .find_map(|a| a.strip_prefix("size="))
            .and_then(|s| s.parse::<u64>().ok())
            .unwrap_or(size);

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

/// Upload one object from the store with `put-object`, then confirm it
/// with `verify-object`.
///
/// Both commands send `size`, plus the `id` and `token` from the batch
/// response's actions: the `upload` action for `put-object`, and the
/// `verify` action for `verify-object` if there is one, else the
/// `upload` action.
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
    // Like upstream, always send `verify-object`, even without a verify
    // action. Upstream always uses the upload action's `id` and `token`.
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
        // Upload and verify lines for the same OID make one
        // ObjectResult with both actions.
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
        // if the server sent them.
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
