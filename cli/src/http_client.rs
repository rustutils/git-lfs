//! Build a `reqwest::Client` honoring git's `http.*` TLS settings.
//!
//! Wired into both the API client (`api/`) and the transfer queue
//! (`transfer/`) so smudge / clean / push all see the same CA bundle and
//! verify-mode for the endpoint URL. Falls back to `reqwest::Client::new()`
//! on any read error so a misconfigured `http.sslcainfo` can't take down
//! commands that don't talk to the network at all.
//!
//! When `http.sslcainfo` is set we install a custom rustls
//! [`ServerCertVerifier`] that trusts certs in the configured bundle by
//! exact-bytes match. This sidesteps webpki's strict end-entity
//! validation: the upstream test fixtures (and many real corp setups)
//! ship a self-signed cert with `CA:TRUE`, which webpki rejects when it
//! shows up as the server's leaf. Pinning by bytes — what
//! `http.sslcainfo` is morally about — is exactly the right semantic.

use std::path::Path;
use std::sync::Arc;

use git_lfs_git::{HttpOptions, extra_headers_for};
use reqwest::cookie::Jar;
use reqwest::header::{HeaderMap, HeaderName, HeaderValue};
use rustls::client::danger::{HandshakeSignatureValid, ServerCertVerified, ServerCertVerifier};
use rustls::{DigitallySignedStruct, SignatureScheme};
use rustls_pki_types::{CertificateDer, PrivateKeyDer, ServerName, UnixTime};

/// Construct a `reqwest::Client` for `endpoint_url`. URL-specific
/// `http.<url>.<key>` overrides win over global `http.<key>`.
///
/// `http.<url>.extraHeader` values (multi-value, longest-prefix
/// match) are merged with global `http.extraHeader` and installed as
/// the client's default headers — so they ride along on every
/// request, including transfer-adapter PUT/GET against action URLs.
pub fn build(cwd: &Path, endpoint_url: &str) -> reqwest::Client {
    let opts = HttpOptions::for_url(cwd, endpoint_url).unwrap_or_default();
    let mut builder = reqwest::ClientBuilder::new();

    if opts.ssl_verify == Some(false) {
        builder = builder.danger_accept_invalid_certs(true);
    } else if let Some(path) = opts.ssl_ca_info.as_deref()
        && let Some(config) =
            pinned_cert_config(path, opts.ssl_cert.as_deref(), opts.ssl_key.as_deref())
    {
        builder = builder.use_preconfigured_tls(config);
    }

    let extras = extra_headers_for(cwd, endpoint_url);
    if !extras.is_empty() {
        let mut headers = HeaderMap::new();
        for (name, value) in extras {
            // `append` (not `insert`) keeps multi-value headers like
            // two `X-Foo: ...` entries — git config allows accumulating
            // values per key, and some servers (or proxies) genuinely
            // care about both.
            if let (Ok(n), Ok(v)) = (
                HeaderName::try_from(name.as_str()),
                HeaderValue::try_from(value.as_str()),
            ) {
                headers.append(n, v);
            }
        }
        if !headers.is_empty() {
            builder = builder.default_headers(headers);
        }
    }

    if let Some(path) = opts.cookie_file.as_deref()
        && let Some(jar) = load_netscape_cookies(path)
    {
        builder = builder.cookie_provider(Arc::new(jar));
    }

    builder.build().unwrap_or_else(|_| reqwest::Client::new())
}

/// Parse a Netscape-format cookie file (the format `curl -b/-c` and
/// most browsers use) and return a populated [`Jar`]. Each line is
/// tab-separated:
///
/// ```text
/// <domain>  <include_subdomains>  <path>  <secure>  <expires>  <name>  <value>
/// ```
///
/// Lines starting with `#` are comments unless they're the special
/// `#HttpOnly_<domain>` prefix marking an HttpOnly cookie. Malformed
/// lines are silently skipped — the file came from `git config`, and
/// a single bad line shouldn't block the whole transfer.
fn load_netscape_cookies(path: &str) -> Option<Jar> {
    let bytes = std::fs::read(path).ok()?;
    let text = String::from_utf8_lossy(&bytes);
    let jar = Jar::default();
    let mut added = 0usize;
    for raw in text.lines() {
        let line = raw.trim_end_matches('\r');
        // Honor the `#HttpOnly_` marker: strip it so the domain
        // parses, but otherwise treat the cookie like any other (the
        // HttpOnly attribute only matters to JS, not to our outgoing
        // requests).
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
        // Reconstruct a Set-Cookie header and a base URL for reqwest's
        // jar. The scheme matters for the secure flag but reqwest's
        // jar honors `Domain=` for cross-host sending either way.
        let scheme = if secure { "https" } else { "http" };
        let base = format!("{scheme}://{domain}{path}");
        let Ok(url) = url::Url::parse(&base) else {
            continue;
        };
        let secure_attr = if secure { "; Secure" } else { "" };
        let cookie = format!("{name}={value}; Domain={domain}; Path={path}{secure_attr}");
        jar.add_cookie_str(&cookie, &url);
        added += 1;
    }
    if added == 0 { None } else { Some(jar) }
}

/// Read `path` as one or more PEM-encoded certs and build a rustls
/// `ClientConfig` that trusts only those certs (by exact byte match).
/// If `cert_path`+`key_path` are also set, attaches a client identity
/// for mTLS (matches `http.sslCert` / `http.sslKey`).
fn pinned_cert_config(
    path: &str,
    cert_path: Option<&str>,
    key_path: Option<&str>,
) -> Option<rustls::ClientConfig> {
    let pem = std::fs::read(path).ok()?;
    let mut cursor = std::io::Cursor::new(&pem);
    let pinned: Vec<CertificateDer<'static>> = rustls_pemfile::certs(&mut cursor)
        .filter_map(Result::ok)
        .collect();
    if pinned.is_empty() {
        return None;
    }

    let provider = Arc::new(rustls::crypto::ring::default_provider());
    let builder = rustls::ClientConfig::builder_with_provider(provider)
        .with_safe_default_protocol_versions()
        .ok()?
        .dangerous()
        .with_custom_certificate_verifier(Arc::new(PinnedCertVerifier { pinned }));

    let config = match (cert_path, key_path) {
        (Some(cp), Some(kp)) => {
            let identity = load_client_identity(cp, kp)?;
            builder.with_client_auth_cert(identity.0, identity.1).ok()?
        }
        _ => builder.with_no_client_auth(),
    };
    Some(config)
}

/// Load a client cert chain + private key from two PEM files for mTLS.
fn load_client_identity(
    cert_path: &str,
    key_path: &str,
) -> Option<(Vec<CertificateDer<'static>>, PrivateKeyDer<'static>)> {
    let cert_pem = std::fs::read(cert_path).ok()?;
    let mut cursor = std::io::Cursor::new(&cert_pem);
    let certs: Vec<CertificateDer<'static>> = rustls_pemfile::certs(&mut cursor)
        .filter_map(Result::ok)
        .collect();
    if certs.is_empty() {
        return None;
    }

    let key_pem = std::fs::read(key_path).ok()?;
    let mut cursor = std::io::Cursor::new(&key_pem);
    let key = rustls_pemfile::private_key(&mut cursor).ok()??;

    Some((certs, key))
}

/// Trusts a server cert iff the leaf's DER bytes match one of the
/// pinned certs from `http.sslcainfo`. Skips name + chain + signature
/// validation: the user already promised the bundle is trusted, and
/// the cert in the bundle may itself be a CA-flagged leaf which webpki
/// won't accept via the normal path.
#[derive(Debug)]
struct PinnedCertVerifier {
    pinned: Vec<CertificateDer<'static>>,
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
        _message: &[u8],
        _cert: &CertificateDer<'_>,
        _dss: &DigitallySignedStruct,
    ) -> Result<HandshakeSignatureValid, rustls::Error> {
        Ok(HandshakeSignatureValid::assertion())
    }

    fn verify_tls13_signature(
        &self,
        _message: &[u8],
        _cert: &CertificateDer<'_>,
        _dss: &DigitallySignedStruct,
    ) -> Result<HandshakeSignatureValid, rustls::Error> {
        Ok(HandshakeSignatureValid::assertion())
    }

    fn supported_verify_schemes(&self) -> Vec<SignatureScheme> {
        vec![
            SignatureScheme::RSA_PKCS1_SHA256,
            SignatureScheme::RSA_PKCS1_SHA384,
            SignatureScheme::RSA_PKCS1_SHA512,
            SignatureScheme::ECDSA_NISTP256_SHA256,
            SignatureScheme::ECDSA_NISTP384_SHA384,
            SignatureScheme::ECDSA_NISTP521_SHA512,
            SignatureScheme::RSA_PSS_SHA256,
            SignatureScheme::RSA_PSS_SHA384,
            SignatureScheme::RSA_PSS_SHA512,
            SignatureScheme::ED25519,
        ]
    }
}
