//! Builds the `reqwest::Client`s for the API client and transfers.
//!
//! Each client gets the `http.*` settings for one URL: `sslVerify`,
//! `sslCAInfo`, `sslCert`, `sslKey`, and `cookieFile`. An encrypted `sslKey`
//! is decrypted with a passphrase from the credential helpers. [`Clients`]
//! looks the settings up for each request's URL, like upstream, so a
//! storage host gets its own settings rather than the LFS endpoint's.
//! `http.extraHeader` is looked up per request too, by the API client and
//! the transfer adapter.
//!
//! With `http.sslCAInfo`, the certificates in the file are the trusted CAs,
//! as upstream. A server certificate that is byte for byte one of them is
//! accepted too, even though webpki rejects a CA as a server certificate:
//! upstream's test server presents its self-signed CA.

use std::collections::HashMap;
use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex, OnceLock};

use git_lfs_creds::{FillContext, Query};
use git_lfs_git::{HttpConfig, HttpOptions};
use reqwest::cookie::Jar;
use rustls::client::WebPkiServerVerifier;
use rustls::client::danger::{HandshakeSignatureValid, ServerCertVerified, ServerCertVerifier};
use rustls::crypto::WebPkiSupportedAlgorithms;
use rustls::{DigitallySignedStruct, SignatureScheme};
use rustls_pki_types::{CertificateDer, PrivateKeyDer, ServerName, UnixTime};

use crate::client_key::{self, EncryptedKey, Key};

/// Build a client with the `http.*` settings for `url`.
pub fn build(cwd: &Path, url: &str) -> reqwest::Client {
    Clients::load(cwd).for_url(url)
}

/// The clients for one command, one for each distinct set of settings.
///
/// The config is read once. Requests whose URLs have the same settings
/// share a client, and with it a connection pool.
pub struct Clients {
    cwd: PathBuf,
    config: HttpConfig,
    cache: Mutex<HashMap<HttpOptions, reqwest::Client>>,
}

impl Clients {
    /// Read the `http.*` settings of the repository at `cwd`.
    ///
    /// Settings that can't be read are skipped, so a bad setting doesn't
    /// break commands that never use the network.
    pub fn load(cwd: &Path) -> Self {
        Self {
            cwd: cwd.to_owned(),
            config: HttpConfig::load(cwd).unwrap_or_default(),
            cache: Mutex::new(HashMap::new()),
        }
    }

    /// The client for a request to `url`.
    ///
    /// `http.<url>.<key>` wins over `http.<key>`.
    pub fn for_url(&self, url: &str) -> reqwest::Client {
        let opts = self.config.options_for(url);
        let mut cache = self.cache.lock().unwrap_or_else(|e| e.into_inner());
        if let Some(client) = cache.get(&opts) {
            return client.clone();
        }
        let client = build_with(&self.cwd, &opts);
        cache.insert(opts, client.clone());
        client
    }
}

/// Build a client with `opts`.
///
/// Files that can't be loaded are skipped.
fn build_with(cwd: &Path, opts: &HttpOptions) -> reqwest::Client {
    if let Some(value) = &opts.ssl_verify_invalid {
        warn_invalid_ssl_verify(value);
    }
    // Redirects are followed by `git_lfs_api::send_with_redirects`.
    let mut builder = reqwest::ClientBuilder::new().redirect(reqwest::redirect::Policy::none());

    // The client certificate doesn't depend on how the server's
    // certificate is checked.
    let identity = opts
        .ssl_cert
        .as_deref()
        .zip(opts.ssl_key.as_deref())
        .and_then(|(cert, key)| load_client_identity(cwd, cert, key));
    let pinned = match opts.ssl_ca_info.as_deref() {
        Some(path) if opts.ssl_verify != Some(false) => {
            pinned_cert_config(path, identity.as_ref().map(clone_identity))
        }
        _ => None,
    };
    if let Some(config) = pinned {
        builder = builder.use_preconfigured_tls(config);
    } else {
        if opts.ssl_verify == Some(false) {
            builder = builder.danger_accept_invalid_certs(true);
        }
        if let Some(identity) = identity {
            match reqwest::Identity::from_pem(&identity_pem(&identity)) {
                Ok(identity) => builder = builder.identity(identity),
                Err(e) => eprintln!("warning: can't use the client certificate: {e}"),
            }
        }
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

/// Build a rustls config that trusts the PEM certificates in `path`, and
/// sends `identity` as the client certificate if there is one.
///
/// Returns `None` if `path` has no certificates.
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
    let mut roots = rustls::RootCertStore::empty();
    roots.add_parsable_certificates(pinned.iter().cloned());
    // None when no certificate in the file can be a trust anchor.
    let webpki = WebPkiServerVerifier::builder_with_provider(Arc::new(roots), provider.clone())
        .build()
        .ok();
    let verifier = CaFileVerifier {
        webpki,
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

fn clone_identity((certs, key): &Identity) -> Identity {
    (certs.clone(), key.clone_key())
}

/// Encode `identity` as PEM, the certificates first, for reqwest.
fn identity_pem((certs, key): &Identity) -> Vec<u8> {
    let mut out = Vec::new();
    for cert in certs {
        pem_section(&mut out, "CERTIFICATE", cert.as_ref());
    }
    let label = match key {
        PrivateKeyDer::Pkcs1(_) => "RSA PRIVATE KEY",
        PrivateKeyDer::Sec1(_) => "EC PRIVATE KEY",
        _ => "PRIVATE KEY",
    };
    pem_section(&mut out, label, key.secret_der());
    out
}

fn pem_section(out: &mut Vec<u8>, label: &str, der: &[u8]) {
    use base64::Engine as _;
    let encoded = base64::engine::general_purpose::STANDARD.encode(der);
    out.extend_from_slice(format!("-----BEGIN {label}-----\n").as_bytes());
    for line in encoded.as_bytes().chunks(64) {
        out.extend_from_slice(line);
        out.push(b'\n');
    }
    out.extend_from_slice(format!("-----END {label}-----\n").as_bytes());
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

/// Accepts a server certificate that chains to a CA in `http.sslCAInfo`,
/// or that is byte for byte one of the file's certificates.
///
/// For an exact match, the host name, chain, and expiry aren't checked, but
/// the handshake signature is, so the server must hold the certificate's
/// private key.
#[derive(Debug)]
struct CaFileVerifier {
    webpki: Option<Arc<WebPkiServerVerifier>>,
    pinned: Vec<CertificateDer<'static>>,
    algorithms: WebPkiSupportedAlgorithms,
}

impl ServerCertVerifier for CaFileVerifier {
    fn verify_server_cert(
        &self,
        end_entity: &CertificateDer<'_>,
        intermediates: &[CertificateDer<'_>],
        server_name: &ServerName<'_>,
        ocsp_response: &[u8],
        now: UnixTime,
    ) -> Result<ServerCertVerified, rustls::Error> {
        let verified = match &self.webpki {
            Some(webpki) => webpki.verify_server_cert(
                end_entity,
                intermediates,
                server_name,
                ocsp_response,
                now,
            ),
            None => Err(rustls::Error::General(
                "no usable CA certificate in http.sslcainfo".into(),
            )),
        };
        if verified.is_err()
            && self
                .pinned
                .iter()
                .any(|p| p.as_ref() == end_entity.as_ref())
        {
            return Ok(ServerCertVerified::assertion());
        }
        verified
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

    // A CA, a certificate for `localhost` it issued, and that certificate's
    // key. Generated once with openssl.
    const CA: &str = "-----BEGIN CERTIFICATE-----
MIIBaTCCARCgAwIBAgIUYUpiZD3PHCtLrr5cUPY+W3ZUUqowCgYIKoZIzj0EAwIw
EjEQMA4GA1UEAwwHVGVzdCBDQTAgFw0yNjExMjUwODI4MjFaGA8yMTI2MTEwMTA4
MjgyMVowEjEQMA4GA1UEAwwHVGVzdCBDQTBZMBMGByqGSM49AgEGCCqGSM49AwEH
A0IABHTdSLqZbjPIJE96q2Ui/FhvvZNlr0EGIL9QW0yAc6y4Ibg0OoUTS4brq7mO
5gha3wWC7lDt2jSyt5oa4ICu9ByjQjBAMB0GA1UdDgQWBBS/GtFvsvGdv8W7ZwTf
KRzEaW3OZjAPBgNVHRMBAf8EBTADAQH/MA4GA1UdDwEB/wQEAwICBDAKBggqhkjO
PQQDAgNHADBEAiAomNYprddcwEt0Gh3pBRS1mQVKyhEUb99S0XiRm1EyNQIgYhS2
0gRuHbJOKp8318jdTS8nN/nn+hrtXWeBM5LIEz0=
-----END CERTIFICATE-----";
    const LEAF: &str = "-----BEGIN CERTIFICATE-----
MIIBYjCCAQigAwIBAgIUNkS9LpJ7u8UBQf7z+woq09IEC9IwCgYIKoZIzj0EAwIw
EjEQMA4GA1UEAwwHVGVzdCBDQTAgFw0yNjExMjUwODI4MjFaGA8yMTI2MTEwMTA4
MjgyMVowFDESMBAGA1UEAwwJbG9jYWxob3N0MFkwEwYHKoZIzj0CAQYIKoZIzj0D
AQcDQgAEfUPD00zw5PJJqYuwpm4V2tsRqdcTnfnp1DZCf0X98bGvKatG+D2SGC9V
OSv5yj7kQmfxWhyFRW0SO5+MqLJmCaM4MDYwCQYDVR0TBAIwADAUBgNVHREEDTAL
gglsb2NhbGhvc3QwEwYDVR0lBAwwCgYIKwYBBQUHAwEwCgYIKoZIzj0EAwIDSAAw
RQIgHvZYCC4/iAo80EMOUI/LySGif7y2YH6e6jk1Z7S+tN4CIQD9GvSbAhO4Gybv
PGxraPF5YLbztQ49lKnntNwOK0cUhg==
-----END CERTIFICATE-----";
    const LEAF_KEY: &str = "-----BEGIN PRIVATE KEY-----
MIGHAgEAMBMGByqGSM49AgEGCCqGSM49AwEHBG0wawIBAQQg2FlXT8bMvR669Npw
VACeLouojjJoT1D5Bxu++bEd5OChRANCAAR9Q8PTTPDk8kmpi7CmbhXa2xGp1xOd
+enUNkJ/Rf3xsa8pq0b4PZIYL1U5K/nKPuRCZ/FaHIVFbRI7n4yosmYJ
-----END PRIVATE KEY-----";

    /// A server that presents a fixed chain and signs with a fixed key.
    #[derive(Debug)]
    struct FixedCert(Arc<CertifiedKey>);

    impl ResolvesServerCert for FixedCert {
        fn resolve(&self, _hello: ClientHello<'_>) -> Option<Arc<CertifiedKey>> {
            Some(self.0.clone())
        }
    }

    fn pem_cert() -> CertificateDer<'static> {
        pem_certs(CERT).remove(0)
    }

    fn pem_certs(pem: &str) -> Vec<CertificateDer<'static>> {
        rustls_pemfile::certs(&mut pem.as_bytes())
            .map(Result::unwrap)
            .collect()
    }

    /// Run a TLS handshake in memory between a client with `ca_info` as
    /// its `http.sslCAInfo`, and a server that presents `chain` and signs
    /// with `server_key`.
    fn handshake(ca_info: &str, chain: &str, server_key: &str) -> Result<(), rustls::Error> {
        let dir = tempfile::tempdir().unwrap();
        let ca = dir.path().join("ca.pem");
        std::fs::write(&ca, ca_info).unwrap();
        let client_config = pinned_cert_config(ca.to_str().unwrap(), None).unwrap();

        let provider = Arc::new(rustls::crypto::ring::default_provider());
        let key = rustls_pemfile::private_key(&mut server_key.as_bytes())
            .unwrap()
            .unwrap();
        let signing_key = provider.key_provider.load_private_key(key).unwrap();
        let certified = CertifiedKey::new(pem_certs(chain), signing_key);
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
    fn identity_pem_is_read_back_by_reqwest() {
        let key = rustls_pemfile::private_key(&mut CERT_KEY.as_bytes())
            .unwrap()
            .unwrap();
        let pem = identity_pem(&(vec![pem_cert()], key));
        let text = String::from_utf8(pem.clone()).unwrap();
        assert!(text.starts_with("-----BEGIN CERTIFICATE-----\n"), "{text}");
        assert!(text.contains("-----BEGIN PRIVATE KEY-----\n"), "{text}");
        reqwest::Identity::from_pem(&pem).unwrap();
    }

    #[test]
    fn pinned_cert_with_its_key_is_accepted() {
        handshake(CERT, CERT, CERT_KEY).unwrap();
    }

    #[test]
    fn pinned_cert_with_another_key_is_rejected() {
        // The certificate is public, so an impostor can present it. Without
        // the private key, its handshake signature must fail.
        assert!(handshake(CERT, CERT, OTHER_KEY).is_err());
    }

    #[test]
    fn cert_issued_by_a_ca_in_the_file_is_accepted() {
        handshake(CA, LEAF, LEAF_KEY).unwrap();
    }

    #[test]
    fn cert_issued_by_another_ca_is_rejected() {
        assert!(handshake(CERT, LEAF, LEAF_KEY).is_err());
    }
}
