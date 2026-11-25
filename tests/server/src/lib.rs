//! A Git and Git LFS server for tests, ported from upstream's
//! `lfstest-gitserver`.
//!
//! [`Server::start`] serves the bare repositories in a directory over Git's
//! smart HTTP protocol, through `git http-backend`, along with the LFS
//! batch, storage, and locking APIs. Objects and locks live in memory.
//!
//! Like upstream's server, it behaves differently for some repository
//! names and object contents, so tests can exercise error handling. For
//! example, an object whose contents are `status-batch-500` gets a 500
//! error in the batch response, and a repository named
//! `requirecreds-...` rejects requests without credentials. The shell
//! tests in `tests/` show what each one is for.
//!
//! Three servers start: plain HTTP ([`Server::url`]), HTTPS
//! ([`Server::ssl_url`]), which also accepts plain HTTP on its port, and
//! HTTPS that requires a client certificate
//! ([`Server::client_cert_url`]). Clients trust the HTTPS servers through
//! the CA in [`Server::cert_pem`].

mod certs;
mod git;
mod http;
mod lfs;
mod locks;
mod state;
mod storage;

use std::io;
use std::path::PathBuf;
use std::sync::{Arc, Mutex, mpsc};
use std::thread::JoinHandle;
use std::time::Duration;

use bytes::Bytes;
use http_body_util::BodyExt;
use hyper::body::Incoming;
use hyper::service::service_fn;
use hyper_util::rt::TokioIo;
use rustls::pki_types::{CertificateDer, PrivateKeyDer, PrivatePkcs8KeyDer};
use rustls::server::WebPkiClientVerifier;
use tokio::io::{AsyncRead, AsyncWrite};
use tokio::net::{TcpListener, TcpStream};
use tokio::sync::watch;
use tokio_rustls::TlsAcceptor;

use crate::certs::Certs;
use crate::http::{Request, Resp, percent_decode, respond, status};
use crate::state::State;

/// A running test server. Dropping it stops the server.
pub struct Server {
    url: String,
    ssl_url: String,
    client_cert_url: String,
    certs: Certs,
    state: Arc<State>,
    stop: watch::Sender<bool>,
    shutdown_requests: mpsc::Receiver<()>,
    thread: Option<JoinHandle<()>>,
}

impl Server {
    /// Start serving the bare repositories in `repo_dir`, named
    /// `<name>.git`.
    ///
    /// The servers listen on 127.0.0.1, on ports the OS picks, and run on
    /// a thread of their own.
    pub fn start(repo_dir: impl Into<PathBuf>) -> io::Result<Self> {
        let repo_dir = repo_dir.into();
        let certs = Certs::generate().map_err(io::Error::other)?;
        let plain_tls = tls_acceptor(&certs, false)?;
        let client_cert_tls = tls_acceptor(&certs, true)?;

        let runtime = tokio::runtime::Builder::new_multi_thread()
            .enable_all()
            .build()?;
        // Bound with std, not tokio, so starting a server works from inside
        // another runtime, such as a `#[tokio::test]`.
        let bind = || {
            let listener = std::net::TcpListener::bind("127.0.0.1:0")?;
            listener.set_nonblocking(true)?;
            Ok::<_, io::Error>(listener)
        };
        let listeners = (bind()?, bind()?, bind()?);
        let addr = |l: &std::net::TcpListener| l.local_addr();
        let url = format!("http://{}", addr(&listeners.0)?);
        let ssl_url = format!("https://{}", addr(&listeners.1)?);
        let client_cert_url = format!("https://{}", addr(&listeners.2)?);

        let state = Arc::new(State::new(repo_dir, url.clone()));
        let (stop, stopped) = watch::channel(false);
        let (request_shutdown, shutdown_requests) = mpsc::channel();
        let shared = Arc::new(Shared {
            state: state.clone(),
            request_shutdown: Mutex::new(request_shutdown),
        });

        let thread = std::thread::spawn(move || {
            runtime.block_on(async move {
                let listen = |l| TcpListener::from_std(l).expect("a nonblocking listener");
                let (plain, dual, client_cert) = (
                    listen(listeners.0),
                    listen(listeners.1),
                    listen(listeners.2),
                );
                let tasks = [
                    tokio::spawn(accept(plain, Mode::Plain, shared.clone(), stopped.clone())),
                    tokio::spawn(accept(
                        dual,
                        Mode::Dual(plain_tls),
                        shared.clone(),
                        stopped.clone(),
                    )),
                    tokio::spawn(accept(
                        client_cert,
                        Mode::Tls(client_cert_tls),
                        shared,
                        stopped,
                    )),
                ];
                for task in tasks {
                    let _ = task.await;
                }
            });
            runtime.shutdown_timeout(Duration::from_secs(1));
        });

        Ok(Self {
            url,
            ssl_url,
            client_cert_url,
            certs,
            state,
            stop,
            shutdown_requests,
            thread: Some(thread),
        })
    }

    /// The plain HTTP server's URL, such as `http://127.0.0.1:1234`.
    pub fn url(&self) -> &str {
        &self.url
    }

    /// The HTTPS server's URL. Its port also accepts plain HTTP.
    pub fn ssl_url(&self) -> &str {
        &self.ssl_url
    }

    /// The URL of the HTTPS server that requires a client certificate.
    pub fn client_cert_url(&self) -> &str {
        &self.client_cert_url
    }

    /// The CA that issued the HTTPS servers' certificate, in PEM. Clients
    /// trust this one.
    pub fn cert_pem(&self) -> String {
        self.certs.server_ca_pem()
    }

    /// A client certificate the client certificate server accepts, in PEM.
    pub fn client_cert_pem(&self) -> &str {
        &self.certs.client_cert_pem
    }

    /// The client certificate's RSA key, in PKCS #1 PEM.
    pub fn client_key_pem(&self) -> &str {
        &self.certs.client_key_pem
    }

    /// The client certificate's key encrypted with the passphrase `pass`,
    /// in OpenSSL's legacy PEM format.
    pub fn client_key_encrypted_pem(&self) -> &str {
        &self.certs.client_key_encrypted_pem
    }

    /// The contents of an object the server has for the repository `repo`,
    /// named without `.git`.
    pub fn object(&self, repo: &str, oid: &str) -> Option<Vec<u8>> {
        self.state.objects.lock().unwrap().get(repo, oid).cloned()
    }

    /// Block until a client sends `POST /shutdown`, as the shell tests do
    /// when they finish.
    pub fn wait_for_shutdown_request(&self) {
        let _ = self.shutdown_requests.recv();
    }
}

impl Drop for Server {
    fn drop(&mut self) {
        let _ = self.stop.send(true);
        if let Some(thread) = self.thread.take() {
            let _ = thread.join();
        }
    }
}

/// What the request handlers share.
struct Shared {
    state: Arc<State>,
    request_shutdown: Mutex<mpsc::Sender<()>>,
}

/// How a listener's connections speak.
enum Mode {
    Plain,
    Tls(TlsAcceptor),
    /// TLS when the first byte starts a TLS handshake, and plain HTTP
    /// otherwise, so a test can redirect from HTTP to HTTPS without
    /// changing ports.
    Dual(TlsAcceptor),
}

fn tls_acceptor(certs: &Certs, require_client_cert: bool) -> io::Result<TlsAcceptor> {
    let provider = Arc::new(rustls::crypto::ring::default_provider());
    let builder = rustls::ServerConfig::builder_with_provider(provider.clone())
        .with_safe_default_protocol_versions()
        .map_err(io::Error::other)?;
    let builder = if require_client_cert {
        let mut roots = rustls::RootCertStore::empty();
        roots
            .add(CertificateDer::from(certs.client_ca_der.clone()))
            .map_err(io::Error::other)?;
        let verifier = WebPkiClientVerifier::builder_with_provider(Arc::new(roots), provider)
            .build()
            .map_err(io::Error::other)?;
        builder.with_client_cert_verifier(verifier)
    } else {
        builder.with_no_client_auth()
    };
    let mut config = builder
        .with_single_cert(
            vec![
                CertificateDer::from(certs.server_cert_der.clone()),
                CertificateDer::from(certs.server_ca_der.clone()),
            ],
            PrivateKeyDer::Pkcs8(PrivatePkcs8KeyDer::from(certs.server_key_der.clone())),
        )
        .map_err(io::Error::other)?;
    config.alpn_protocols = vec![b"http/1.1".to_vec()];
    Ok(TlsAcceptor::from(Arc::new(config)))
}

async fn accept(
    listener: TcpListener,
    mode: Mode,
    shared: Arc<Shared>,
    mut stopped: watch::Receiver<bool>,
) {
    let mode = Arc::new(mode);
    loop {
        let stream = tokio::select! {
            accepted = listener.accept() => match accepted {
                Ok((stream, _)) => stream,
                Err(_) => continue,
            },
            _ = stopped.changed() => return,
        };
        let mode = mode.clone();
        let shared = shared.clone();
        tokio::spawn(async move {
            match &*mode {
                Mode::Plain => serve(stream, shared).await,
                Mode::Tls(tls) => {
                    if let Ok(stream) = tls.accept(stream).await {
                        serve(stream, shared).await;
                    }
                }
                Mode::Dual(tls) => {
                    if starts_tls(&stream).await {
                        if let Ok(stream) = tls.accept(stream).await {
                            serve(stream, shared).await;
                        }
                    } else {
                        serve(stream, shared).await;
                    }
                }
            }
        });
    }
}

/// Whether the client's first byte is a TLS handshake record (RFC 8446,
/// section 5.1). Waits up to 5 seconds for it.
async fn starts_tls(stream: &TcpStream) -> bool {
    let mut first = [0u8; 1];
    matches!(
        tokio::time::timeout(Duration::from_secs(5), stream.peek(&mut first)).await,
        Ok(Ok(1))
    ) && first[0] == 0x16
}

async fn serve<S>(stream: S, shared: Arc<Shared>)
where
    S: AsyncRead + AsyncWrite + Unpin + Send + 'static,
{
    let service = service_fn(move |req| {
        let shared = shared.clone();
        async move { Ok::<_, hyper::Error>(handle(&shared, req).await) }
    });
    let _ = hyper::server::conn::http1::Builder::new()
        .title_case_headers(true)
        .serve_connection(TokioIo::new(stream), service)
        .await;
}

async fn handle(shared: &Shared, req: hyper::Request<Incoming>) -> Resp {
    let (parts, body) = req.into_parts();
    let body = match body.collect().await {
        Ok(b) => b.to_bytes(),
        Err(_) => Bytes::new(),
    };
    let req = Request {
        method: parts.method.as_str().to_owned(),
        path: percent_decode(parts.uri.path()),
        query: parts.uri.query().unwrap_or("").to_owned(),
        headers: parts.headers,
        body,
    };
    let state = &shared.state;
    eprintln!("{} {}", req.method, req.url_string());

    let path = req.path.as_str();
    match path {
        "/shutdown" => {
            let _ = shared.request_shutdown.lock().unwrap().send(());
            status(200)
        }
        "/verify" => lfs::verify(state, &req),
        "/status" => respond(200, &[], format!("{:?}\n", std::time::SystemTime::now())),
        _ if path.starts_with("/storage/") => storage::handle(state, &req),
        _ if path.starts_with("/redirect307/") => lfs::redirect307(state, &req),
        _ if path.starts_with("/limits/") => lfs::limits(state, &req),
        _ => lfs::root(state, &req).await,
    }
}
