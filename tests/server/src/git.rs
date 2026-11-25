//! Git's smart HTTP protocol, through `git http-backend`.

use std::process::Stdio;

use tokio::io::AsyncWriteExt;
use tokio::process::Command;

use crate::http::{Request, Resp, add_header, error, respond};
use crate::state::State;

/// Run `git http-backend` as a CGI script for `req`.
///
/// Like upstream's server, the environment has only the CGI variables, so
/// the user's Git config doesn't apply.
pub(crate) async fn http_backend(state: &State, req: &Request) -> Resp {
    let mut cmd = Command::new("git");
    cmd.arg("http-backend")
        .env_clear()
        .env("GIT_PROJECT_ROOT", &state.repo_dir)
        .env("GIT_HTTP_EXPORT_ALL", "")
        .env("PATH_INFO", &req.path)
        .env("QUERY_STRING", &req.query)
        .env("REQUEST_METHOD", &req.method)
        .env("CONTENT_TYPE", req.header("Content-Type"));
    let protocols: Vec<_> = req.headers.get_all("Git-Protocol").iter().collect();
    if let [protocol] = protocols.as_slice()
        && let Ok(protocol) = protocol.to_str()
    {
        cmd.env("GIT_PROTOCOL", protocol);
    }
    cmd.stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::inherit());

    let mut child = match cmd.spawn() {
        Ok(c) => c,
        Err(e) => return error(500, &format!("git http-backend: {e}")),
    };
    let mut stdin = child.stdin.take().expect("piped");
    let body = req.body.clone();
    // Write the body while reading the output, so neither side blocks on
    // a full pipe.
    let writer = tokio::spawn(async move {
        let _ = stdin.write_all(&body).await;
    });
    let output = match child.wait_with_output().await {
        Ok(o) => o,
        Err(e) => return error(500, &format!("git http-backend: {e}")),
    };
    let _ = writer.await;
    backend_response(&output)
}

/// The response for `git http-backend`'s output.
///
/// It exits 0 even for a missing repository, so a failure with no output
/// means it didn't run, as when Git is installed without it. Answering 200
/// then would make Git fall back to the dumb HTTP protocol, and only fail
/// later, on push.
fn backend_response(output: &std::process::Output) -> Resp {
    if !output.status.success() && output.stdout.is_empty() {
        return error(500, "git http-backend failed without output");
    }
    cgi_response(&output.stdout)
}

/// Turn CGI output, headers then a blank line then the body, into a
/// response, the way upstream's server does.
///
/// Upstream reads the first line as an HTTP status line, which CGI output
/// never has, so it drops that line and always answers 200. The tests rely
/// on that: for a missing repository, the dropped line is `Status: 404`,
/// and a clone of it gets an empty repository rather than an error.
fn cgi_response(out: &[u8]) -> Resp {
    let (head, body) = match find(out, b"\r\n\r\n") {
        Some(i) => (&out[..i], &out[i + 4..]),
        None => match find(out, b"\n\n") {
            Some(i) => (&out[..i], &out[i + 2..]),
            None => (out, &[][..]),
        },
    };
    let head = String::from_utf8_lossy(head);
    let mut resp = respond(200, &[], body.to_vec());
    for line in head.lines().skip(1) {
        if let Some((name, value)) = line.split_once(':') {
            add_header(&mut resp, name, value.trim());
        }
    }
    resp
}

fn find(haystack: &[u8], needle: &[u8]) -> Option<usize> {
    haystack.windows(needle.len()).position(|w| w == needle)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn cgi_first_line_is_dropped_and_status_is_ok() {
        let resp = cgi_response(b"Status: 404 Not Found\r\nContent-Type: text/plain\r\n\r\nnope");
        assert_eq!(resp.status(), 200);
        assert_eq!(resp.headers()["content-type"], "text/plain");
        assert!(!resp.headers().contains_key("status"));
    }

    fn output(code: i32, stdout: &[u8]) -> std::process::Output {
        use std::os::unix::process::ExitStatusExt;
        std::process::Output {
            status: std::process::ExitStatus::from_raw(code << 8),
            stdout: stdout.to_vec(),
            stderr: Vec::new(),
        }
    }

    #[test]
    fn a_backend_that_did_not_run_is_an_error() {
        // What `git http-backend` gives when Git doesn't have it.
        assert_eq!(backend_response(&output(1, b"")).status(), 500);
    }

    #[test]
    fn a_missing_repository_is_still_ok() {
        let out = output(0, b"Status: 404 Not Found\r\n\r\nNot a git repository");
        assert_eq!(backend_response(&out).status(), 200);
    }
}
