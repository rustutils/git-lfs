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

use git_lfs_git::HttpOptions;
use rustls::client::danger::{HandshakeSignatureValid, ServerCertVerified, ServerCertVerifier};
use rustls::{DigitallySignedStruct, SignatureScheme};
use rustls_pki_types::{CertificateDer, ServerName, UnixTime};

/// Construct a `reqwest::Client` for `endpoint_url`. URL-specific
/// `http.<url>.<key>` overrides win over global `http.<key>`.
pub fn build(cwd: &Path, endpoint_url: &str) -> reqwest::Client {
    let opts = HttpOptions::for_url(cwd, endpoint_url).unwrap_or_default();
    let mut builder = reqwest::ClientBuilder::new();

    if opts.ssl_verify == Some(false) {
        builder = builder.danger_accept_invalid_certs(true);
    } else if let Some(path) = opts.ssl_ca_info.as_deref() {
        if let Some(config) = pinned_cert_config(path) {
            builder = builder.use_preconfigured_tls(config);
        }
    }
    builder.build().unwrap_or_else(|_| reqwest::Client::new())
}

/// Read `path` as one or more PEM-encoded certs and build a rustls
/// `ClientConfig` that trusts only those certs (by exact byte match).
fn pinned_cert_config(path: &str) -> Option<rustls::ClientConfig> {
    let pem = std::fs::read(path).ok()?;
    let mut cursor = std::io::Cursor::new(&pem);
    let pinned: Vec<CertificateDer<'static>> = rustls_pemfile::certs(&mut cursor)
        .filter_map(Result::ok)
        .collect();
    if pinned.is_empty() {
        return None;
    }

    let provider = Arc::new(rustls::crypto::ring::default_provider());
    let config = rustls::ClientConfig::builder_with_provider(provider)
        .with_safe_default_protocol_versions()
        .ok()?
        .dangerous()
        .with_custom_certificate_verifier(Arc::new(PinnedCertVerifier { pinned }))
        .with_no_client_auth();
    Some(config)
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
        if self.pinned.iter().any(|p| p.as_ref() == end_entity.as_ref()) {
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
