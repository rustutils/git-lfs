//! Builds the `reqwest::Client` shared by the API client and transfers.
//!
//! The client gets the `http.*` settings for the endpoint URL: `sslVerify`,
//! `sslCAInfo`, `sslCert`, `sslKey`, and `cookieFile`. An encrypted
//! `sslKey` is decrypted with a passphrase from the credential helpers. It uses them for every
//! request, including action URLs on other hosts. Upstream looks the settings
//! up for each request's URL. `http.extraHeader` is looked up per request,
//! by the API client and the transfer adapter.
//!
//! With `http.sslCAInfo`, a server certificate is accepted only if it's
//! byte for byte one of the certificates in the file. That accepts the
//! self-signed `CA:TRUE` certificate the upstream test server uses, which
//! webpki rejects as a server certificate, but it also rejects a server
//! certificate signed by a CA in the file.

use std::collections::HashMap;
use std::path::Path;
use std::sync::{Arc, Mutex, OnceLock};

use git_lfs_creds::{FillContext, Query};
use git_lfs_git::HttpOptions;
use reqwest::cookie::Jar;
use rustls::client::danger::{HandshakeSignatureValid, ServerCertVerified, ServerCertVerifier};
use rustls::crypto::WebPkiSupportedAlgorithms;
use rustls::{DigitallySignedStruct, SignatureScheme};
use rustls_pki_types::{CertificateDer, PrivateKeyDer, ServerName, UnixTime};

use crate::client_key::{self, EncryptedKey, Key};

/// Build a client with the `http.*` settings for `endpoint_url`.
///
/// `http.<url>.<key>` wins over `http.<key>`, except for `extraHeader`,
/// where all matching values are sent. Settings that can't be read, and
/// files that can't be loaded, are skipped, so a bad setting doesn't break
/// commands that never use the network.
pub fn build(cwd: &Path, endpoint_url: &str) -> reqwest::Client {
    let opts = HttpOptions::for_url(cwd, endpoint_url).unwrap_or_default();
    if let Some(value) = &opts.ssl_verify_invalid {
        warn_invalid_ssl_verify(value);
    }
    // Redirects are followed by `git_lfs_api::send_with_redirects`.
    let mut builder = reqwest::ClientBuilder::new().redirect(reqwest::redirect::Policy::none());

    if opts.ssl_verify == Some(false) {
        builder = builder.danger_accept_invalid_certs(true);
    } else if let Some(path) = opts.ssl_ca_info.as_deref()
        && let Some(config) = pinned_cert_config(
            path,
            opts.ssl_cert
                .as_deref()
                .zip(opts.ssl_key.as_deref())
                .and_then(|(cert, key)| load_client_identity(cwd, cert, key)),
        )
    {
        builder = builder.use_preconfigured_tls(config);
    }

    if let Some(path) = opts.cookie_file.as_deref()
        && let Some(jar) = load_netscape_cookies(path)
    {
        builder = builder.cookie_provider(Arc::new(jar));
    }

    builder
        .build()
        .unwrap_or_else(|_| git_lfs_api::http_client())
}

/// Warn, once per process, that `http.sslVerify` isn't a boolean.
///
/// Commands build more than one client, so this would otherwise repeat.
fn warn_invalid_ssl_verify(value: &str) {
    static WARNED: std::sync::atomic::AtomicBool = std::sync::atomic::AtomicBool::new(false);
    if !WARNED.swap(true, std::sync::atomic::Ordering::Relaxed) {
        eprintln!(
            "warning: http.sslVerify has the invalid value {value:?}, so certificates are still verified"
        );
    }
}

/// Load a Netscape-format cookie file, as written by curl.
///
/// Each line has seven tab-separated fields:
///
/// ```text
/// <domain>  <include_subdomains>  <path>  <secure>  <expires>  <name>  <value>
/// ```
///
/// Lines starting with `#` are comments, except for the `#HttpOnly_`
/// prefix on a domain. Malformed lines are skipped. The subdomain and
/// expiry fields are ignored. Returns `None` if the file can't be read.
///
/// Even an empty file gives a jar, which then keeps the cookies that
/// responses set, as upstream does.
fn load_netscape_cookies(path: &str) -> Option<Jar> {
    let bytes = std::fs::read(path).ok()?;
    let text = String::from_utf8_lossy(&bytes);
    let jar = Jar::default();
    for raw in text.lines() {
        let line = raw.trim_end_matches('\r');
        // HttpOnly only matters to browsers, so the cookie is used like any
        // other.
        let line = line.strip_prefix("#HttpOnly_").unwrap_or(line);
        if line.is_empty() || line.starts_with('#') {
            continue;
        }
        let fields: Vec<&str> = line.split('\t').collect();
        if fields.len() < 7 {
            continue;
        }
        let domain = fields[0].trim_start_matches('.');
        let path = fields[2];
        let secure = fields[3].eq_ignore_ascii_case("TRUE");
        let name = fields[5];
        let value = fields[6];
        if name.is_empty() || domain.is_empty() {
            continue;
        }
        // The jar takes a Set-Cookie header and the URL it came from.
        // `Domain=` is always set, so subdomains always get the cookie.
        let scheme = if secure { "https" } else { "http" };
        let base = format!("{scheme}://{domain}{path}");
        let Ok(url) = url::Url::parse(&base) else {
            continue;
        };
        let secure_attr = if secure { "; Secure" } else { "" };
        let cookie = format!("{name}={value}; Domain={domain}; Path={path}{secure_attr}");
        jar.add_cookie_str(&cookie, &url);
    }
    Some(jar)
}

/// A client certificate chain and its private key.
type Identity = (Vec<CertificateDer<'static>>, PrivateKeyDer<'static>);

/// Build a rustls config that trusts exactly the PEM certificates in
/// `path`, and sends `identity` as the client certificate if there is one.
///
/// The client certificate settings have no effect without
/// `http.sslCAInfo`. Returns `None` if `path` has no certificates.
fn pinned_cert_config(path: &str, identity: Option<Identity>) -> Option<rustls::ClientConfig> {
    let pem = std::fs::read(path).ok()?;
    let mut cursor = std::io::Cursor::new(&pem);
    let pinned: Vec<CertificateDer<'static>> = rustls_pemfile::certs(&mut cursor)
        .filter_map(Result::ok)
        .collect();
    if pinned.is_empty() {
        return None;
    }

    let provider = Arc::new(rustls::crypto::ring::default_provider());
    let verifier = PinnedCertVerifier {
        pinned,
        algorithms: provider.signature_verification_algorithms,
    };
    let builder = rustls::ClientConfig::builder_with_provider(provider)
        .with_safe_default_protocol_versions()
        .ok()?
        .dangerous()
        .with_custom_certificate_verifier(Arc::new(verifier));

    let config = match identity {
        Some((certs, key)) => builder.with_client_auth_cert(certs, key).ok()?,
        None => builder.with_no_client_auth(),
    };
    Some(config)
}

/// Load a client certificate chain and private key from two PEM files
/// (`http.sslCert` and `http.sslKey`).
fn load_client_identity(cwd: &Path, cert_path: &str, key_path: &str) -> Option<Identity> {
    let cert_pem = std::fs::read(cert_path).ok()?;
    let mut cursor = std::io::Cursor::new(&cert_pem);
    let certs: Vec<CertificateDer<'static>> = rustls_pemfile::certs(&mut cursor)
        .filter_map(Result::ok)
        .collect();
    if certs.is_empty() {
        return None;
    }
    Some((certs, load_client_key(cwd, key_path)?))
}

/// Load the private key at `path`, asking for its passphrase if it's
/// encrypted.
///
/// The key is kept for the rest of the process, since a command builds
/// more than one client and the passphrase should be asked for once.
fn load_client_key(cwd: &Path, path: &str) -> Option<PrivateKeyDer<'static>> {
    type Cache = HashMap<String, Option<PrivateKeyDer<'static>>>;
    static KEYS: OnceLock<Mutex<Cache>> = OnceLock::new();
    let mut keys = KEYS
        .get_or_init(Default::default)
        .lock()
        .unwrap_or_else(|e| e.into_inner());
    let key = keys
        .entry(path.to_owned())
        .or_insert_with(|| read_client_key(cwd, path));
    key.as_ref().map(PrivateKeyDer::clone_key)
}

fn read_client_key(cwd: &Path, path: &str) -> Option<PrivateKeyDer<'static>> {
    let pem = std::fs::read(path).ok()?;
    let result = match client_key::parse(&pem) {
        Ok(Key::Plain(key)) => Ok(key),
        Ok(Key::Encrypted(key)) => decrypt_client_key(cwd, path, &key),
        Err(e) => Err(e.to_string()),
    };
    result
        .inspect_err(|e| eprintln!("warning: can't use client key file {path:?}: {e}"))
        .ok()
}

/// Decrypt `key` with the passphrase from the credential helpers, as
/// upstream does.
///
/// The helpers get `protocol=cert` and the key's path, like Git's query
/// for `http.sslCertPasswordProtected`. The passphrase is approved if it
/// works, and rejected if it doesn't.
fn decrypt_client_key(
    cwd: &Path,
    path: &str,
    key: &EncryptedKey,
) -> Result<PrivateKeyDer<'static>, String> {
    let query = Query {
        protocol: "cert".to_owned(),
        host: String::new(),
        path: path.replace('\\', "/"),
    };
    let url = url::Url::parse(&format!("cert:///{}", query.path)).map_err(|e| e.to_string())?;
    let helpers = crate::fetcher::default_helper_chain(cwd, &url);
    let ctx = FillContext {
        username: Some(String::new()),
        ..FillContext::default()
    };
    let creds = helpers
        .fill_with_context(&query, &ctx)
        .map_err(|e| e.to_string())?
        .ok_or("no passphrase from the credential helpers")?;
    match key.decrypt(creds.password.as_bytes()) {
        Ok(der) => {
            let _ = helpers.approve(&query, &creds);
            Ok(der)
        }
        Err(e) => {
            let _ = helpers.reject(&query, &creds);
            Err(e.to_string())
        }
    }
}

/// Accepts a server certificate only if its bytes match a pinned one.
///
/// The host name, chain, and expiry aren't checked, but the handshake
/// signature is, so the server must hold the certificate's private key.
/// Pinning exact bytes, rather than trusting the file as CAs like upstream,
/// accepts self-signed CA certificates that rustls's own verifier rejects as
/// server certificates.
#[derive(Debug)]
struct PinnedCertVerifier {
    pinned: Vec<CertificateDer<'static>>,
    algorithms: WebPkiSupportedAlgorithms,
}

impl ServerCertVerifier for PinnedCertVerifier {
    fn verify_server_cert(
        &self,
        end_entity: &CertificateDer<'_>,
        _intermediates: &[CertificateDer<'_>],
        _server_name: &ServerName<'_>,
        _ocsp_response: &[u8],
        _now: UnixTime,
    ) -> Result<ServerCertVerified, rustls::Error> {
        if self
            .pinned
            .iter()
            .any(|p| p.as_ref() == end_entity.as_ref())
        {
            Ok(ServerCertVerified::assertion())
        } else {
            Err(rustls::Error::General(
                "presented certificate does not match http.sslcainfo".into(),
            ))
        }
    }

    fn verify_tls12_signature(
        &self,
        message: &[u8],
        cert: &CertificateDer<'_>,
        dss: &DigitallySignedStruct,
    ) -> Result<HandshakeSignatureValid, rustls::Error> {
        rustls::crypto::verify_tls12_signature(message, cert, dss, &self.algorithms)
    }

    fn verify_tls13_signature(
        &self,
        message: &[u8],
        cert: &CertificateDer<'_>,
        dss: &DigitallySignedStruct,
    ) -> Result<HandshakeSignatureValid, rustls::Error> {
        rustls::crypto::verify_tls13_signature(message, cert, dss, &self.algorithms)
    }

    fn supported_verify_schemes(&self) -> Vec<SignatureScheme> {
        self.algorithms.supported_schemes()
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use rustls::server::{ClientHello, ResolvesServerCert};
    use rustls::sign::CertifiedKey;

    // A self-signed certificate for `localhost`, its key, and an unrelated
    // key. Generated once with openssl.
    const CERT: &str = "-----BEGIN CERTIFICATE-----
MIIBdDCCARqgAwIBAgIUbDDaPQe0JitNWNZtyXPaOF8VhrMwCgYIKoZIzj0EAwIw
FDESMBAGA1UEAwwJbG9jYWxob3N0MCAXDTI2MTEyMTEyMzAwNFoYDzIxMjYxMDI4
MTIzMDA0WjAUMRIwEAYDVQQDDAlsb2NhbGhvc3QwWTATBgcqhkjOPQIBBggqhkjO
PQMBBwNCAASMWY/4opDRYJtPjAFg8JCeWyef3IQS/o705xYB7usuXqPw8QTl0vOw
+v5FymCgO6j6vxo99PqSy1+8h3DaDfhJo0gwRjAdBgNVHQ4EFgQUN7Gi76lonwhi
7t/nVioYnt3Ly8wwDwYDVR0TAQH/BAUwAwEB/zAUBgNVHREEDTALgglsb2NhbGhv
c3QwCgYIKoZIzj0EAwIDSAAwRQIhAPSzpgjN85fZfsHoQ9VH/zCwwsQXB+eVQswq
9odyYJl3AiBLQgdiX2lsZPf7gxQ86yskFV8a8UjefmA2BI1poMNzzQ==
-----END CERTIFICATE-----";
    const CERT_KEY: &str = "-----BEGIN PRIVATE KEY-----
MIGHAgEAMBMGByqGSM49AgEGCCqGSM49AwEHBG0wawIBAQQgiL/YQe/UK/XFaiYJ
3RvAJZRBbrEtn+eSiiZemVGAcRuhRANCAASMWY/4opDRYJtPjAFg8JCeWyef3IQS
/o705xYB7usuXqPw8QTl0vOw+v5FymCgO6j6vxo99PqSy1+8h3DaDfhJ
-----END PRIVATE KEY-----";
    const OTHER_KEY: &str = "-----BEGIN PRIVATE KEY-----
MIGHAgEAMBMGByqGSM49AgEGCCqGSM49AwEHBG0wawIBAQQgaX/mkUA00glo6KTc
d+T6QhWlixzjH5Yp4XkTBs/T9j6hRANCAAQzd5YbwUeACPHItktSLJL9/T2sDHMa
gK5S8+YxJvFLdXrxncZ47Udpx0Y7Hf9UmASWvx2Tfgnth0/rVfoPtZUq
-----END PRIVATE KEY-----";

    /// A server that presents `CERT` and signs with the given key.
    #[derive(Debug)]
    struct FixedCert(Arc<CertifiedKey>);

    impl ResolvesServerCert for FixedCert {
        fn resolve(&self, _hello: ClientHello<'_>) -> Option<Arc<CertifiedKey>> {
            Some(self.0.clone())
        }
    }

    fn pem_cert() -> CertificateDer<'static> {
        rustls_pemfile::certs(&mut CERT.as_bytes())
            .next()
            .unwrap()
            .unwrap()
    }

    /// Run a TLS handshake in memory between a client that pins `CERT`
    /// and a server that presents `CERT` but signs with `server_key`.
    fn handshake(server_key: &str) -> Result<(), rustls::Error> {
        let dir = tempfile::tempdir().unwrap();
        let ca = dir.path().join("ca.pem");
        std::fs::write(&ca, CERT).unwrap();
        let client_config = pinned_cert_config(ca.to_str().unwrap(), None).unwrap();

        let provider = Arc::new(rustls::crypto::ring::default_provider());
        let key = rustls_pemfile::private_key(&mut server_key.as_bytes())
            .unwrap()
            .unwrap();
        let signing_key = provider.key_provider.load_private_key(key).unwrap();
        let certified = CertifiedKey::new(vec![pem_cert()], signing_key);
        let server_config = rustls::ServerConfig::builder_with_provider(provider)
            .with_safe_default_protocol_versions()
            .unwrap()
            .with_no_client_auth()
            .with_cert_resolver(Arc::new(FixedCert(Arc::new(certified))));

        let mut client = rustls::ClientConnection::new(
            Arc::new(client_config),
            ServerName::try_from("localhost").unwrap(),
        )?;
        let mut server = rustls::ServerConnection::new(Arc::new(server_config))?;
        while client.is_handshaking() || server.is_handshaking() {
            let mut buf = Vec::new();
            client.write_tls(&mut buf).unwrap();
            server.read_tls(&mut buf.as_slice()).unwrap();
            server.process_new_packets()?;
            let mut buf = Vec::new();
            server.write_tls(&mut buf).unwrap();
            client.read_tls(&mut buf.as_slice()).unwrap();
            client.process_new_packets()?;
        }
        Ok(())
    }

    #[test]
    fn pinned_cert_with_its_key_is_accepted() {
        handshake(CERT_KEY).unwrap();
    }

    #[test]
    fn pinned_cert_with_another_key_is_rejected() {
        // The certificate is public, so an impostor can present it. Without
        // the private key, its handshake signature must fail.
        assert!(handshake(OTHER_KEY).is_err());
    }
}
