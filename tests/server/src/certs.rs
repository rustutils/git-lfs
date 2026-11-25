//! The certificates and keys the TLS servers use and the tests read.

use std::net::{IpAddr, Ipv4Addr, Ipv6Addr};

use aes::cipher::block_padding::Pkcs7;
use aes::cipher::{BlockModeEncrypt, KeyIvInit};
use base64::Engine as _;
use md5::{Digest, Md5};
use rcgen::{
    BasicConstraints, CertificateParams, DistinguishedName, DnType, ExtendedKeyUsagePurpose, IsCa,
    KeyPair, KeyUsagePurpose, PKCS_ECDSA_P256_SHA256, PKCS_RSA_SHA256, SanType,
};
use rsa::pkcs1::EncodeRsaPrivateKey;
use rsa::pkcs8::EncodePrivateKey;
use rsa::rand_core::{OsRng, RngCore};

/// What the servers present and what a client needs to talk to them.
pub(crate) struct Certs {
    /// The CA that issued the HTTPS servers' certificate, in DER. Clients
    /// trust this one.
    pub server_ca_der: Vec<u8>,
    /// The HTTPS servers' certificate, in DER.
    ///
    /// Go's test certificate is self-signed and also a CA, but rustls
    /// rejects a CA as a server's certificate, so this one is a leaf.
    pub server_cert_der: Vec<u8>,
    /// The server certificate's private key, in PKCS #8 DER.
    pub server_key_der: Vec<u8>,
    /// The CA that issued the client certificate, in DER.
    pub client_ca_der: Vec<u8>,
    /// The client certificate, in PEM.
    pub client_cert_pem: String,
    /// The client certificate's RSA key, in PKCS #1 PEM.
    pub client_key_pem: String,
    /// The same key encrypted with the passphrase `pass`, in the legacy
    /// OpenSSL format with `Proc-Type` and `DEK-Info` headers.
    pub client_key_encrypted_pem: String,
}

impl Certs {
    pub(crate) fn generate() -> Result<Self, rcgen::Error> {
        let server_ca_key = KeyPair::generate_for(&PKCS_ECDSA_P256_SHA256)?;
        let mut server_ca = CertificateParams::default();
        server_ca.distinguished_name = organization("Acme Co");
        server_ca.is_ca = IsCa::Ca(BasicConstraints::Unconstrained);
        server_ca.key_usages = vec![
            KeyUsagePurpose::DigitalSignature,
            KeyUsagePurpose::KeyCertSign,
        ];
        let server_ca_cert = server_ca.self_signed(&server_ca_key)?;

        let server_key = KeyPair::generate_for(&PKCS_ECDSA_P256_SHA256)?;
        let mut server = CertificateParams::default();
        // A name of its own: OpenSSL takes a certificate whose subject is
        // its issuer's for a self-signed one.
        server.distinguished_name = organization("Acme Co server");
        server.key_usages = vec![KeyUsagePurpose::DigitalSignature];
        server.extended_key_usages = vec![ExtendedKeyUsagePurpose::ServerAuth];
        server.subject_alt_names = vec![
            SanType::IpAddress(IpAddr::V4(Ipv4Addr::LOCALHOST)),
            SanType::IpAddress(IpAddr::V6(Ipv6Addr::LOCALHOST)),
            SanType::DnsName("example.com".try_into()?),
            SanType::DnsName("*.example.com".try_into()?),
        ];
        let server_cert = server.signed_by(&server_key, &server_ca_cert, &server_ca_key)?;

        let ca_key = KeyPair::generate_for(&PKCS_ECDSA_P256_SHA256)?;
        let mut ca = CertificateParams::default();
        ca.distinguished_name = organization("Yhat, Inc.");
        ca.is_ca = IsCa::Ca(BasicConstraints::Unconstrained);
        ca.key_usages = vec![
            KeyUsagePurpose::KeyCertSign,
            KeyUsagePurpose::DigitalSignature,
        ];
        ca.extended_key_usages = vec![
            ExtendedKeyUsagePurpose::ServerAuth,
            ExtendedKeyUsagePurpose::ClientAuth,
        ];
        let ca_cert = ca.self_signed(&ca_key)?;

        // The client key is RSA, written as PKCS #1, as upstream's is: that's
        // the format the encrypted key tests exercise.
        let rsa_key = rsa::RsaPrivateKey::new(&mut OsRng, 2048)
            .map_err(|_| rcgen::Error::KeyGenerationUnavailable)?;
        let pkcs8 = rsa_key
            .to_pkcs8_der()
            .map_err(|_| rcgen::Error::KeyGenerationUnavailable)?;
        let pkcs1 = rsa_key
            .to_pkcs1_der()
            .map_err(|_| rcgen::Error::KeyGenerationUnavailable)?;
        let client_key =
            KeyPair::from_pkcs8_der_and_sign_algo(&pkcs8.as_bytes().into(), &PKCS_RSA_SHA256)?;
        let mut client = CertificateParams::default();
        client.distinguished_name = organization("Yhat, Inc.");
        client.key_usages = vec![KeyUsagePurpose::DigitalSignature];
        client.extended_key_usages = vec![ExtendedKeyUsagePurpose::ClientAuth];
        let client_cert = client.signed_by(&client_key, &ca_cert, &ca_key)?;

        Ok(Self {
            server_ca_der: server_ca_cert.der().to_vec(),
            server_cert_der: server_cert.der().to_vec(),
            server_key_der: server_key.serialize_der(),
            client_ca_der: ca_cert.der().to_vec(),
            client_cert_pem: client_cert.pem(),
            client_key_pem: pem("RSA PRIVATE KEY", &[], pkcs1.as_bytes()),
            client_key_encrypted_pem: encrypt_legacy_pem(pkcs1.as_bytes(), b"pass"),
        })
    }

    /// The server certificate's CA in PEM, which clients add to their
    /// trusted CAs.
    pub(crate) fn server_ca_pem(&self) -> String {
        pem("CERTIFICATE", &[], &self.server_ca_der)
    }
}

fn organization(name: &str) -> DistinguishedName {
    let mut dn = DistinguishedName::new();
    dn.push(DnType::OrganizationName, name);
    dn
}

/// Encrypt a PKCS #1 RSA key with AES-128-CBC, as Go's
/// `x509.EncryptPEMBlock` and OpenSSL's legacy format do.
///
/// The key is the MD5 of the passphrase followed by the first 8 bytes of
/// the IV. The IV is written in uppercase hex, which GnuTLS 3.7 requires.
fn encrypt_legacy_pem(der: &[u8], passphrase: &[u8]) -> String {
    let mut iv = [0u8; 16];
    OsRng.fill_bytes(&mut iv);
    let mut md5 = Md5::new();
    md5.update(passphrase);
    md5.update(&iv[..8]);
    let key = md5.finalize();
    // Room for up to a block of padding.
    let mut buf = der.to_vec();
    buf.resize(der.len() + 16, 0);
    let encrypted = cbc::Encryptor::<aes::Aes128>::new_from_slices(&key, &iv)
        .expect("16-byte key and IV")
        .encrypt_padded::<Pkcs7>(&mut buf, der.len())
        .expect("the buffer has room for the padding")
        .to_vec();
    let iv_hex: String = iv.iter().map(|b| format!("{b:02X}")).collect();
    pem(
        "RSA PRIVATE KEY",
        &[
            ("Proc-Type", "4,ENCRYPTED"),
            ("DEK-Info", &format!("AES-128-CBC,{iv_hex}")),
        ],
        &encrypted,
    )
}

/// Encode `der` as a PEM block with the given headers.
fn pem(label: &str, headers: &[(&str, &str)], der: &[u8]) -> String {
    let mut out = format!("-----BEGIN {label}-----\n");
    for (name, value) in headers {
        out.push_str(&format!("{name}: {value}\n"));
    }
    if !headers.is_empty() {
        out.push('\n');
    }
    let encoded = base64::engine::general_purpose::STANDARD.encode(der);
    for line in encoded.as_bytes().chunks(64) {
        out.push_str(std::str::from_utf8(line).expect("base64 is ASCII"));
        out.push('\n');
    }
    out.push_str(&format!("-----END {label}-----\n"));
    out
}

#[cfg(test)]
mod tests {
    use aes::cipher::BlockModeDecrypt;

    use super::*;

    #[test]
    fn encrypted_key_decrypts_with_the_passphrase() {
        let der = b"not really a key, but any bytes will do";
        let pem = encrypt_legacy_pem(der, b"pass");
        let iv_hex = pem
            .lines()
            .find_map(|l| l.strip_prefix("DEK-Info: AES-128-CBC,"))
            .unwrap();
        assert!(!iv_hex.chars().any(|c| c.is_ascii_lowercase()));
        let iv: Vec<u8> = (0..16)
            .map(|i| u8::from_str_radix(&iv_hex[i * 2..i * 2 + 2], 16).unwrap())
            .collect();
        let body: String = pem
            .lines()
            .skip_while(|l| !l.is_empty())
            .filter(|l| !l.is_empty() && !l.starts_with("-----"))
            .collect();
        let mut encrypted = base64::engine::general_purpose::STANDARD
            .decode(body)
            .unwrap();
        let mut md5 = Md5::new();
        md5.update(b"pass");
        md5.update(&iv[..8]);
        let key = md5.finalize();
        let plain = cbc::Decryptor::<aes::Aes128>::new_from_slices(&key, &iv)
            .unwrap()
            .decrypt_padded::<Pkcs7>(&mut encrypted)
            .unwrap();
        assert_eq!(plain, der);
    }
}
