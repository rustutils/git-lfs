//! Builds the `reqwest::Client` shared by the API client and transfers.
//!
//! The client gets the `http.*` settings for the endpoint URL:
//! `sslVerify`, `sslCAInfo`, `sslCert`, `sslKey`, `extraHeader`, and
//! `cookieFile`. It uses them for every request, including action URLs on
//! other hosts. Upstream looks the settings up for each request's URL.
//!
//! With `http.sslCAInfo`, a server certificate is accepted only if it's
//! byte for byte one of the certificates in the file. That accepts the
//! self-signed `CA:TRUE` certificate the upstream test server uses, which
//! webpki rejects as a server certificate, but it also rejects a server
//! certificate signed by a CA in the file.

use std::path::Path;
use std::sync::Arc;

use git_lfs_git::{HttpOptions, extra_headers_for};
use reqwest::cookie::Jar;
use reqwest::header::{HeaderMap, HeaderName, HeaderValue};
use rustls::client::danger::{HandshakeSignatureValid, ServerCertVerified, ServerCertVerifier};
use rustls::{DigitallySignedStruct, SignatureScheme};
use rustls_pki_types::{CertificateDer, PrivateKeyDer, ServerName, UnixTime};

/// Build a client with the `http.*` settings for `endpoint_url`.
///
/// `http.<url>.<key>` wins over `http.<key>`, except for `extraHeader`,
/// where all matching values are sent. Settings that can't be read, and
/// files that can't be loaded, are skipped, so a bad setting doesn't break
/// commands that never use the network.
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
            // `append` keeps every value of a header set more than once.
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
/// expiry fields are ignored. Returns `None` if the file can't be read or
/// has no cookies.
fn load_netscape_cookies(path: &str) -> Option<Jar> {
    let bytes = std::fs::read(path).ok()?;
    let text = String::from_utf8_lossy(&bytes);
    let jar = Jar::default();
    let mut added = 0usize;
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
        added += 1;
    }
    if added == 0 { None } else { Some(jar) }
}

/// Build a rustls config that trusts exactly the PEM certificates in
/// `path`.
///
/// With `cert_path` and `key_path` (`http.sslCert` and `http.sslKey`), it
/// also sends a client certificate. Those two settings have no effect
/// without `http.sslCAInfo`. Returns `None` if `path` has no certificates.
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

/// Load a client certificate chain and private key from two PEM files.
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

/// Accepts a server certificate only if its bytes match a pinned one.
///
/// The host name, chain, and expiry aren't checked. The handshake
/// signature isn't checked either, so the server never proves it holds the
/// certificate's private key.
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
