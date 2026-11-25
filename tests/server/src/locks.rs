//! The locking API: create, list, verify, and unlock.

use std::time::SystemTime;

use rsa::rand_core::{OsRng, RngCore};
use serde::{Deserialize, Serialize};

use crate::http::{Request, Resp, add_header, json_line, lfs_error, not_found, respond, status};
use crate::lfs::{RefName, check_required_branch, extract_auth, ref_name};
use crate::state::{Lock, State, User, hex, rfc3339};

const JSON: &str = "application/json";

/// A list of locks. Go encodes a missing list as `null`, so this does too.
#[derive(Serialize)]
struct LockList {
    locks: Option<Vec<Lock>>,
    #[serde(skip_serializing_if = "String::is_empty")]
    next_cursor: String,
    #[serde(skip_serializing_if = "String::is_empty")]
    message: String,
}

#[derive(Serialize)]
struct VerifiableLockList {
    ours: Option<Vec<Lock>>,
    theirs: Option<Vec<Lock>>,
    #[serde(skip_serializing_if = "String::is_empty")]
    next_cursor: String,
    #[serde(skip_serializing_if = "String::is_empty")]
    message: String,
}

#[derive(Serialize)]
struct LockResponse {
    lock: Option<Lock>,
    #[serde(skip_serializing_if = "String::is_empty")]
    message: String,
}

impl LockResponse {
    fn message(message: impl Into<String>) -> String {
        json_line(&LockResponse {
            lock: None,
            message: message.into(),
        })
    }
}

#[derive(Deserialize)]
struct LockRequest {
    #[serde(default)]
    path: String,
    #[serde(default, rename = "ref")]
    r#ref: Option<RefName>,
}

#[derive(Deserialize)]
struct UnlockRequest {
    #[serde(default, rename = "ref")]
    r#ref: Option<RefName>,
}

#[derive(Deserialize)]
struct VerifyRequest {
    #[serde(default, rename = "ref")]
    r#ref: Option<RefName>,
    #[serde(default)]
    cursor: Option<String>,
    #[serde(default)]
    limit: Option<i64>,
}

pub(crate) fn handle(state: &State, req: &Request, repo: &str) -> Resp {
    let checking_lock = req.header("X-Check-Lock") == "1";
    if !checking_lock && repo == "locksunsupported" {
        return status(404);
    }

    if repo == "netrctest" {
        let bad = match extract_auth(req.header("Authorization")) {
            Ok((_, user, pass)) => user == "netrcuser" && pass == "badpassretry",
            Err(_) => true,
        };
        if bad {
            return lfs_error(401, "Error: Bad Auth");
        }
    }

    if req.method != "POST" {
        return list(state, req, repo);
    }
    let mut resp = if req.path.ends_with("unlock") {
        unlock(state, req, repo)
    } else if req.path.ends_with("/locks/verify") {
        verify(state, req, repo)
    } else if req.path.ends_with("/locks") {
        create(state, req, repo)
    } else {
        not_found()
    };
    // Go sets the type before handling any POST.
    if !resp.headers().contains_key("content-type") {
        add_header(&mut resp, "Content-Type", JSON);
    }
    resp
}

fn list(state: &State, req: &Request, repo: &str) -> Resp {
    let path = req.path.strip_suffix('/').unwrap_or(&req.path);
    if !path.ends_with("/locks") {
        let body = format!("{{\"message\":\"unknown path: {}\"}}", req.path);
        return respond(404, &[("Content-Type", JSON)], body);
    }
    if let Some(resp) = check_required_branch(repo, &req.query_param("refspec")) {
        return resp;
    }
    let mut list = LockList {
        locks: None,
        next_cursor: String::new(),
        message: String::new(),
    };
    match filtered_locks(
        state,
        repo,
        &req.query_param("path"),
        &req.query_param("cursor"),
        &req.query_param("limit"),
    ) {
        Ok((locks, next)) => {
            list.locks = locks;
            list.next_cursor = next;
        }
        Err(e) => list.message = e,
    }
    respond(200, &[("Content-Type", JSON)], json_line(&list))
}

/// A repository's locks, from `cursor` on, at `path` if it's set.
///
/// Like upstream's test server, a limit gives pages of at most 3 locks
/// whatever its value, and the lock after each page is skipped: the next
/// cursor names the lock after it.
fn filtered_locks(
    state: &State,
    repo: &str,
    path: &str,
    cursor: &str,
    limit: &str,
) -> Result<(Option<Vec<Lock>>, String), String> {
    let mut locks = state
        .locks
        .lock()
        .unwrap()
        .get(repo)
        .cloned()
        .unwrap_or_default();
    if !cursor.is_empty() {
        match locks.iter().position(|l| l.id == cursor) {
            Some(i) => {
                locks.drain(..i);
            }
            None => return Err(format!("cursor ({cursor}) not found")),
        }
    }
    let mut locks = Some(locks);
    if !path.is_empty() {
        let filtered: Vec<Lock> = locks
            .take()
            .unwrap_or_default()
            .into_iter()
            .filter(|l| l.path == path)
            .collect();
        // Go's filter starts from a nil slice.
        locks = (!filtered.is_empty()).then_some(filtered);
    }
    if !limit.is_empty() {
        if limit.parse::<i64>().is_err() {
            return Err("unable to parse limit amount".to_owned());
        }
        let all = locks.clone().unwrap_or_default();
        let size = all.len().min(3);
        if size + 1 < all.len() {
            let next = all[size + 1].id.clone();
            return Ok((Some(all[..size].to_vec()), next));
        }
    }
    Ok((locks, String::new()))
}

fn create(state: &State, req: &Request, repo: &str) -> Resp {
    let json = |body: String| respond(200, &[("Content-Type", JSON)], body);
    let request: LockRequest = match serde_json::from_slice(&req.body) {
        Ok(r) => r,
        Err(e) => return json(LockResponse::message(e.to_string())),
    };
    if let Some(resp) = check_required_branch(repo, ref_name(&request.r#ref)) {
        return resp;
    }

    let mut all = state.locks.lock().unwrap();
    let locks = all.entry(repo.to_owned()).or_default();
    if locks.iter().any(|l| l.path == request.path) {
        return json(LockResponse::message("lock already created"));
    }
    let mut id = [0u8; 20];
    OsRng.fill_bytes(&mut id);
    let now = SystemTime::now();
    let lock = Lock {
        id: hex(&id),
        path: request.path,
        owner: User {
            name: "Git LFS Tests".to_owned(),
        },
        locked_at: rfc3339(now),
        created: now,
    };
    locks.push(lock.clone());
    locks.sort_by_key(|l| l.created);
    json(json_line(&LockResponse {
        lock: Some(lock),
        message: String::new(),
    }))
}

fn unlock(state: &State, req: &Request, repo: &str) -> Resp {
    let json = |body: String| respond(200, &[("Content-Type", JSON)], body);
    let id = req
        .path
        .strip_suffix("/unlock")
        .and_then(|p| p.rsplit_once("locks/"))
        .map(|(_, id)| id)
        .filter(|id| !id.is_empty() && !id.contains('/'));
    let Some(id) = id else {
        return json(LockResponse::message("Invalid lock"));
    };
    let request: UnlockRequest = match serde_json::from_slice(&req.body) {
        Ok(r) => r,
        Err(e) => return json(LockResponse::message(e.to_string())),
    };
    if let Some(resp) = check_required_branch(repo, ref_name(&request.r#ref)) {
        return resp;
    }

    let mut all = state.locks.lock().unwrap();
    let locks = all.entry(repo.to_owned()).or_default();
    match locks.iter().position(|l| l.id == id) {
        Some(i) => {
            let lock = locks.remove(i);
            json(json_line(&LockResponse {
                lock: Some(lock),
                message: String::new(),
            }))
        }
        None => json(LockResponse::message("unable to find lock")),
    }
}

fn verify(state: &State, req: &Request, repo: &str) -> Resp {
    for (suffix, code) in [
        ("verify-5xx", 500),
        ("verify-501", 501),
        ("verify-403", 403),
    ] {
        if repo.ends_with(suffix) {
            return status(code);
        }
    }
    match repo {
        "pre_push_locks_verify_404" | "pre_push_locks_verify_410" => {
            let code = if repo.ends_with("404") { 404 } else { 410 };
            let body = format!("{{\"message\":\"{repo}\"}}");
            return respond(code, &[("Content-Type", JSON)], body);
        }
        _ => {}
    }

    let request: VerifyRequest = match serde_json::from_slice(&req.body) {
        Ok(r) => r,
        Err(e) => {
            let message = format!("json decode error: {e}");
            return respond(
                400,
                &[("Content-Type", JSON)],
                json_line(&serde_json::json!({ "message": message })),
            );
        }
    };
    if let Some(resp) = check_required_branch(repo, ref_name(&request.r#ref)) {
        return resp;
    }

    let mut list = VerifiableLockList {
        ours: None,
        theirs: None,
        next_cursor: String::new(),
        message: String::new(),
    };
    // Go formats the limit even when it's 0, so verify always pages.
    let limit = request.limit.unwrap_or(0).to_string();
    match filtered_locks(
        state,
        repo,
        "",
        request.cursor.as_deref().unwrap_or(""),
        &limit,
    ) {
        Ok((locks, next)) => {
            list.next_cursor = next;
            for lock in locks.unwrap_or_default() {
                let side = if lock.path.contains("theirs") {
                    &mut list.theirs
                } else {
                    &mut list.ours
                };
                side.get_or_insert_with(Vec::new).push(lock);
            }
        }
        Err(e) => list.message = e,
    }
    respond(200, &[("Content-Type", JSON)], json_line(&list))
}
