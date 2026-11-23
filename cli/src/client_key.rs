//! Private keys for client certificates, including keys encrypted with a
//! passphrase in OpenSSL's legacy PEM format.
//!
//! An encrypted key has `Proc-Type: 4,ENCRYPTED` and `DEK-Info` headers,
//! as `openssl rsa -aes128` writes them:
//!
//! ```text
//! -----BEGIN RSA PRIVATE KEY-----
//! Proc-Type: 4,ENCRYPTED
//! DEK-Info: AES-128-CBC,<IV in hex>
//!
//! <base64>
//! -----END RSA PRIVATE KEY-----
//! ```
//!
//! The cipher key comes from the passphrase and the first 8 bytes of the
//! IV through OpenSSL's `EVP_BytesToKey` with MD5. Like upstream, which
//! uses Go's `x509.DecryptPEMBlock`, this supports DES, triple DES, and
//! AES in CBC mode. PKCS #8 encrypted keys (`ENCRYPTED PRIVATE KEY`)
//! aren't supported.

use aes::cipher::block_padding::Pkcs7;
use aes::cipher::{BlockCipherDecrypt, BlockModeDecrypt, KeyInit, KeyIvInit};
use base64::Engine as _;
use md5::{Digest, Md5};
use rustls_pki_types::{PrivateKeyDer, PrivatePkcs1KeyDer, PrivatePkcs8KeyDer, PrivateSec1KeyDer};

/// Why a key file couldn't be loaded.
#[derive(Debug, thiserror::Error, PartialEq, Eq)]
pub enum KeyError {
    #[error("no PEM private key found")]
    NoKey,
    #[error("malformed encrypted PEM block: {0}")]
    Malformed(&'static str),
    #[error("unsupported cipher {0:?}")]
    UnsupportedCipher(String),
    #[error("decryption password incorrect")]
    WrongPassphrase,
}

/// A private key read from PEM.
pub enum Key {
    Plain(PrivateKeyDer<'static>),
    Encrypted(EncryptedKey),
}

/// A legacy encrypted PEM key, waiting for its passphrase.
pub struct EncryptedKey {
    label: String,
    cipher: Cipher,
    iv: Vec<u8>,
    data: Vec<u8>,
}

/// Read the first private key in `pem`.
pub fn parse(pem: &[u8]) -> Result<Key, KeyError> {
    if let Some(encrypted) = parse_encrypted(pem)? {
        return Ok(Key::Encrypted(encrypted));
    }
    match rustls_pemfile::private_key(&mut &pem[..]) {
        Ok(Some(key)) => Ok(Key::Plain(key)),
        _ => Err(KeyError::NoKey),
    }
}

impl EncryptedKey {
    /// Decrypt the key with `passphrase`.
    ///
    /// A wrong passphrase usually leaves bad padding, which gives
    /// [`KeyError::WrongPassphrase`]. In rare cases the padding happens to
    /// be valid, and the key is rejected later when it's used.
    pub fn decrypt(&self, passphrase: &[u8]) -> Result<PrivateKeyDer<'static>, KeyError> {
        let key = derive_key(passphrase, &self.iv[..8], self.cipher.key_len());
        let der = self
            .cipher
            .decrypt(&key, &self.iv, &self.data)
            .ok_or(KeyError::WrongPassphrase)?;
        Ok(match self.label.as_str() {
            "RSA PRIVATE KEY" => PrivatePkcs1KeyDer::from(der).into(),
            "EC PRIVATE KEY" => PrivateSec1KeyDer::from(der).into(),
            _ => PrivatePkcs8KeyDer::from(der).into(),
        })
    }
}

#[derive(Clone, Copy, Debug)]
enum Cipher {
    Des,
    Des3,
    Aes128,
    Aes192,
    Aes256,
}

impl Cipher {
    fn from_name(name: &str) -> Option<Self> {
        Some(match name {
            "DES-CBC" => Self::Des,
            "DES-EDE3-CBC" => Self::Des3,
            "AES-128-CBC" => Self::Aes128,
            "AES-192-CBC" => Self::Aes192,
            "AES-256-CBC" => Self::Aes256,
            _ => return None,
        })
    }

    fn key_len(self) -> usize {
        match self {
            Self::Des => 8,
            Self::Aes128 => 16,
            Self::Des3 | Self::Aes192 => 24,
            Self::Aes256 => 32,
        }
    }

    fn block_len(self) -> usize {
        match self {
            Self::Des | Self::Des3 => 8,
            Self::Aes128 | Self::Aes192 | Self::Aes256 => 16,
        }
    }

    /// Decrypt CBC `data` and strip its padding, or `None` if the padding
    /// is bad.
    fn decrypt(self, key: &[u8], iv: &[u8], data: &[u8]) -> Option<Vec<u8>> {
        fn cbc<C: BlockCipherDecrypt + KeyInit>(
            key: &[u8],
            iv: &[u8],
            data: &[u8],
        ) -> Option<Vec<u8>> {
            let mut buf = data.to_vec();
            let len = cbc::Decryptor::<C>::new_from_slices(key, iv)
                .ok()?
                .decrypt_padded::<Pkcs7>(&mut buf)
                .ok()?
                .len();
            buf.truncate(len);
            Some(buf)
        }
        match self {
            Self::Des => cbc::<des::Des>(key, iv, data),
            Self::Des3 => cbc::<des::TdesEde3>(key, iv, data),
            Self::Aes128 => cbc::<aes::Aes128>(key, iv, data),
            Self::Aes192 => cbc::<aes::Aes192>(key, iv, data),
            Self::Aes256 => cbc::<aes::Aes256>(key, iv, data),
        }
    }
}

/// OpenSSL's `EVP_BytesToKey` with MD5 and one round, as legacy PEM
/// encryption uses.
fn derive_key(passphrase: &[u8], salt: &[u8], len: usize) -> Vec<u8> {
    let mut out = Vec::with_capacity(len);
    let mut digest: Vec<u8> = Vec::new();
    while out.len() < len {
        let mut md5 = Md5::new();
        md5.update(&digest);
        md5.update(passphrase);
        md5.update(salt);
        digest = md5.finalize().to_vec();
        out.extend_from_slice(&digest);
    }
    out.truncate(len);
    out
}

/// Find a PEM block with `Proc-Type: 4,ENCRYPTED`.
///
/// Returns `None` when the first block isn't encrypted.
fn parse_encrypted(pem: &[u8]) -> Result<Option<EncryptedKey>, KeyError> {
    let text = String::from_utf8_lossy(pem);
    let mut lines = text.lines().map(str::trim_end);
    let Some(label) = lines.find_map(|l| {
        l.strip_prefix("-----BEGIN ")
            .and_then(|l| l.strip_suffix("-----"))
    }) else {
        return Ok(None);
    };
    let label = label.to_owned();

    let mut headers = Vec::new();
    let mut body = String::new();
    let mut in_headers = true;
    for line in lines {
        if line.starts_with("-----END ") {
            break;
        }
        if in_headers {
            if line.is_empty() {
                in_headers = false;
                continue;
            }
            if let Some((k, v)) = line.split_once(':') {
                headers.push((k.trim().to_owned(), v.trim().to_owned()));
                continue;
            }
            in_headers = false;
        }
        body.push_str(line.trim());
    }

    let header = |name: &str| {
        headers
            .iter()
            .find(|(k, _)| k == name)
            .map(|(_, v)| v.as_str())
    };
    if header("Proc-Type") != Some("4,ENCRYPTED") {
        return Ok(None);
    }
    let dek = header("DEK-Info").ok_or(KeyError::Malformed("no DEK-Info header"))?;
    let (name, iv_hex) = dek
        .split_once(',')
        .ok_or(KeyError::Malformed("malformed DEK-Info header"))?;
    let cipher =
        Cipher::from_name(name).ok_or_else(|| KeyError::UnsupportedCipher(name.to_owned()))?;
    let iv = decode_hex(iv_hex).ok_or(KeyError::Malformed("malformed IV"))?;
    if iv.len() != cipher.block_len() {
        return Err(KeyError::Malformed("IV has the wrong length"));
    }
    let data = base64::engine::general_purpose::STANDARD
        .decode(&body)
        .map_err(|_| KeyError::Malformed("bad base64"))?;
    if data.is_empty() || data.len() % cipher.block_len() != 0 {
        return Err(KeyError::Malformed("data isn't a whole number of blocks"));
    }
    Ok(Some(EncryptedKey {
        label,
        cipher,
        iv,
        data,
    }))
}

fn decode_hex(s: &str) -> Option<Vec<u8>> {
    if !s.len().is_multiple_of(2) {
        return None;
    }
    (0..s.len())
        .step_by(2)
        .map(|i| u8::from_str_radix(s.get(i..i + 2)?, 16).ok())
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    // One EC key, unencrypted and encrypted with the passphrase `pass`,
    // written by `openssl ec -aes128` and `openssl ec -des3`.
    const PLAIN: &str = "-----BEGIN EC PRIVATE KEY-----
MHcCAQEEILpxYZQpL627enr112TWXerBb7pzwHFVxw4IYRdgjU1QoAoGCCqGSM49
AwEHoUQDQgAEi60YvGRh9py9spytSXD0ysTXwP0UwJ+WT2BTRvfFGK9ZtTOQoGCf
aw02ZrtM6S1zqBxm8d1kzO7alB3evX5LHQ==
-----END EC PRIVATE KEY-----
";
    const AES: &str = "-----BEGIN EC PRIVATE KEY-----
Proc-Type: 4,ENCRYPTED
DEK-Info: AES-128-CBC,5D429F2CB50593423FE445A37D4A46CF

y884mNEnM2ZB8Vq/2Mux+94oPLD31G8c/aOH5WTdPGjE5jCqJo3nRV9N+IiKgc7x
yS8nB70QRNG0YMjggr7fLcqT9fyYgdmNw4QF8bIKflBNfFCFYFjxCSZSWQXwJc+s
qXZM3mU70uuLPRQldwv5rnFfrep6Bm9wGmKn79M5oYA=
-----END EC PRIVATE KEY-----
";
    const DES3: &str = "-----BEGIN EC PRIVATE KEY-----
Proc-Type: 4,ENCRYPTED
DEK-Info: DES-EDE3-CBC,17B572EC0680FAA1

qfnAyifWhlIoZrsqLiSKsFJ2Tkeo8hOaoJNxgwo0wC3WA0vwS40v5sQcM2UlAjTf
U70e/oe6/JsvgWrriH8u/9unOJyFKQ6gX/HA3v4eKcq9DCg2A8weS95m6jO5RfOI
vTTRWtLK1rHw9/Wi0XA93shkHwdzkh4AiUonBdFQkjo=
-----END EC PRIVATE KEY-----
";

    fn plain() -> PrivateKeyDer<'static> {
        match parse(PLAIN.as_bytes()).unwrap() {
            Key::Plain(k) => k,
            Key::Encrypted(_) => panic!("expected a plain key"),
        }
    }

    fn encrypted(pem: &str) -> EncryptedKey {
        match parse(pem.as_bytes()).unwrap() {
            Key::Encrypted(k) => k,
            Key::Plain(_) => panic!("expected an encrypted key"),
        }
    }

    #[test]
    fn decrypts_aes_and_triple_des_keys() {
        for pem in [AES, DES3] {
            let key = encrypted(pem).decrypt(b"pass").unwrap();
            assert_eq!(key, plain());
        }
    }

    #[test]
    fn wrong_passphrase_is_an_error() {
        assert_eq!(
            encrypted(AES).decrypt(b"wrong").err(),
            Some(KeyError::WrongPassphrase)
        );
    }

    #[test]
    fn missing_key_is_an_error() {
        assert_eq!(parse(b"nothing here").err(), Some(KeyError::NoKey));
    }

    #[test]
    fn unknown_cipher_is_an_error() {
        let pem = "-----BEGIN RSA PRIVATE KEY-----\nProc-Type: 4,ENCRYPTED\nDEK-Info: CAMELLIA-128-CBC,00\n\nAAAA\n-----END RSA PRIVATE KEY-----\n";
        assert_eq!(
            parse(pem.as_bytes()).err(),
            Some(KeyError::UnsupportedCipher("CAMELLIA-128-CBC".into()))
        );
    }
}
