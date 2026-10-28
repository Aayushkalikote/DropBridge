//! Quick Share QR codes, for phones that don't show up on their own.
//!
//! Scanning the code makes the phone advertise its Quick Share service over mDNS with a
//! "QR code" TLV record in its endpoint info. A hidden phone puts its name there, encrypted
//! with a key derived from the code. Protocol notes: NearDrop's PROTOCOL.md.

use std::path::Path;

use aes_gcm::aead::{Aead, KeyInit, Payload};
use aes_gcm::{Aes128Gcm, Nonce};
use anyhow::{Context, Result};
use base64::Engine;
use hkdf::Hkdf;
use p256::elliptic_curve::sec1::ToEncodedPoint;
use sha2::Sha256;

pub struct Key {
    /// 2-byte version (0) + compressed P-256 public key; the `key=` value of the URL.
    data: Vec<u8>,
    token: [u8; 16],
    name_key: [u8; 16],
}

impl Key {
    fn new(data: Vec<u8>) -> Self {
        let hk = Hkdf::<Sha256>::new(None, &data);
        let (mut token, mut name_key) = ([0u8; 16], [0u8; 16]);
        hk.expand(b"advertisingContext", &mut token).expect("16 bytes is a valid HKDF length");
        hk.expand(b"encryptionKey", &mut name_key).expect("16 bytes is a valid HKDF length");
        Self { data, token, name_key }
    }

    /// What a visible phone that scanned this code puts in its QR code record.
    pub fn token(&self) -> &[u8] {
        &self.token
    }

    pub fn url(&self) -> String {
        let key = base64::engine::general_purpose::URL_SAFE_NO_PAD.encode(&self.data);
        format!("https://quickshare.google/qrcode#key={key}")
    }

    /// Name of a hidden phone from its QR code record: 12-byte nonce, then AES-GCM
    /// ciphertext and tag, authenticated with the advertising token.
    pub fn name(&self, record: &[u8]) -> Option<String> {
        if record.len() <= 12 + 16 {
            return None; // a visible phone sends just the 16-byte token; its name is in plain text
        }
        let (nonce, msg) = record.split_at(12);
        let nonce: [u8; 12] = nonce.try_into().ok()?;
        let cipher = Aes128Gcm::new(&self.name_key.into());
        let plain = cipher.decrypt(&Nonce::from(nonce), Payload { msg, aad: &self.token }).ok()?;
        String::from_utf8(plain).ok().filter(|n| !n.is_empty())
    }
}

/// The QR key in `dir/qr.key`, created on first use. It is kept so the printed code stays
/// valid while the background service runs.
// shortcut: only the public half is stored, so phones still ask Accept; keep the private key
// too once rqs_lib can sign `qr_code_handshake_data`.
pub fn load(dir: &Path) -> Result<Key> {
    let path = dir.join("qr.key");
    if let Ok(data) = std::fs::read(&path) {
        if data.len() == 35 {
            return Ok(Key::new(data));
        }
    }
    let secret = loop {
        let mut b = [0u8; 32];
        // SAFETY: fills exactly b.len() bytes.
        unsafe { libc::arc4random_buf(b.as_mut_ptr().cast(), b.len()) };
        if let Ok(s) = p256::SecretKey::from_slice(&b) {
            break s;
        }
    };
    let mut data = vec![0, 0];
    data.extend_from_slice(secret.public_key().to_encoded_point(true).as_bytes());
    std::fs::create_dir_all(dir)?;
    std::fs::write(&path, &data).with_context(|| format!("writing {}", path.display()))?;
    Ok(Key::new(data))
}

/// The code as text: dark modules on a forced white background, so it scans in dark terminals too.
pub fn render(url: &str) -> Result<String> {
    let code = qrcode::QrCode::new(url)?;
    let art = code.render::<qrcode::render::unicode::Dense1x2>().quiet_zone(true).build();
    Ok(art.lines().map(|l| format!("\x1b[30;47m{l}\x1b[0m\n")).collect())
}

#[cfg(test)]
pub mod tests {
    use super::*;

    pub fn key() -> Key {
        let mut data = vec![0, 0, 2];
        data.extend([7u8; 32]);
        Key::new(data)
    }

    pub fn other_key() -> Key {
        let mut data = vec![0, 0, 3];
        data.extend([8u8; 32]);
        Key::new(data)
    }

    /// A QR code record as a hidden phone sends it.
    pub fn encrypt(k: &Key, name: &[u8]) -> Vec<u8> {
        let nonce = [9u8; 12];
        let ct = Aes128Gcm::new(&k.name_key.into())
            .encrypt(&Nonce::from(nonce), Payload { msg: name, aad: &k.token })
            .unwrap();
        [&nonce[..], &ct].concat()
    }

    #[test]
    fn url_carries_versioned_compressed_key() {
        let dir = std::env::temp_dir().join(format!("dropbridge-qr-{}", std::process::id()));
        let k = load(&dir).unwrap();
        assert_eq!(k.data.len(), 35);
        assert_eq!(&k.data[..2], &[0, 0]);
        assert!(matches!(k.data[2], 2 | 3));
        assert_eq!(load(&dir).unwrap().data, k.data, "kept across loads");
        assert_eq!(k.url().len(), "https://quickshare.google/qrcode#key=".len() + 47);
        std::fs::remove_dir_all(dir).unwrap();
    }

    #[test]
    fn decrypts_hidden_name() {
        let k = key();
        let record = encrypt(&k, b"Galaxy A56");
        assert_eq!(k.name(&record).as_deref(), Some("Galaxy A56"));
        assert_eq!(k.name(&k.token), None);
        let mut bad = record.clone();
        bad[20] ^= 1;
        assert_eq!(k.name(&bad), None);
    }
}
