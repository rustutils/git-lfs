//! `lfstest-gitserver`: the test server for upstream's shell tests.
//!
//! Writes the servers' URLs, certificate, and client certificate and keys
//! to the files named by the `LFSTEST_*` variables, serves the
//! repositories in `LFSTEST_DIR` until a client sends `POST /shutdown`,
//! then removes the files.

use std::path::PathBuf;
use std::process::ExitCode;

use lfstest_server::Server;

fn main() -> ExitCode {
    let repo_dir = std::env::var_os("LFSTEST_DIR").unwrap_or_default();
    let server = match Server::start(repo_dir) {
        Ok(s) => s,
        Err(e) => {
            eprintln!("lfstest-gitserver: {e}");
            return ExitCode::FAILURE;
        }
    };

    let files = [
        ("LFSTEST_URL", "lfstest-gitserver", server.url().to_owned()),
        (
            "LFSTEST_SSL_URL",
            "lfstest-gitserver-ssl",
            server.ssl_url().to_owned(),
        ),
        (
            "LFSTEST_CLIENT_CERT_URL",
            "lfstest-gitserver-client-cert-url",
            server.client_cert_url().to_owned(),
        ),
        ("LFSTEST_CERT", "lfstest-gitserver-cert", server.cert_pem()),
        (
            "LFSTEST_CLIENT_CERT",
            "lfstest-gitserver-client-cert",
            server.client_cert_pem().to_owned(),
        ),
        (
            "LFSTEST_CLIENT_KEY",
            "lfstest-gitserver-client-key",
            server.client_key_pem().to_owned(),
        ),
        (
            "LFSTEST_CLIENT_KEY_ENCRYPTED",
            "lfstest-gitserver-client-key-enc",
            server.client_key_encrypted_pem().to_owned(),
        ),
    ];
    let mut written = Vec::new();
    for (var, default, contents) in files {
        let path = std::env::var_os(var)
            .filter(|v| !v.is_empty())
            .map_or_else(|| PathBuf::from(default), PathBuf::from);
        if let Err(e) = std::fs::write(&path, contents) {
            eprintln!("lfstest-gitserver: writing {}: {e}", path.display());
            return ExitCode::FAILURE;
        }
        written.push(path);
    }
    eprintln!("server url: {}", server.url());
    eprintln!("server tls url: {}", server.ssl_url());
    eprintln!("server client cert url: {}", server.client_cert_url());

    server.wait_for_shutdown_request();
    drop(server);
    for path in written {
        let _ = std::fs::remove_file(path);
    }
    ExitCode::SUCCESS
}
