//! Self-signed TLS identity for the AirDrop listeners. AirDrop senders accept any
//! certificate in "Everyone" mode; RSA matches what sharingd already accepted from OpenDrop.

use std::path::Path;
use std::sync::Arc;

use anyhow::{bail, Context, Result};
use tokio_rustls::TlsAcceptor;

pub fn acceptor(dir: &Path) -> Result<TlsAcceptor> {
    let (cert, key) = (dir.join("cert.pem"), dir.join("key.pem"));
    if !cert.exists() || !key.exists() {
        std::fs::create_dir_all(dir)?;
        let status = std::process::Command::new("/usr/bin/openssl")
            // -addext forces an X.509 v3 cert; LibreSSL makes v1 otherwise, which rustls rejects.
            .args(["req", "-newkey", "rsa:2048", "-nodes", "-x509", "-days", "3650", "-subj", "/CN=dropbridge"])
            .args(["-addext", "basicConstraints=CA:FALSE"])
            .arg("-keyout")
            .arg(&key)
            .arg("-out")
            .arg(&cert)
            .stderr(std::process::Stdio::null())
            .status()
            .context("running /usr/bin/openssl")?;
        if !status.success() {
            bail!("openssl failed to create a certificate");
        }
    }
    let certs = rustls_pemfile::certs(&mut std::io::BufReader::new(std::fs::File::open(&cert)?))
        .collect::<Result<Vec<_>, _>>()?;
    let key = rustls_pemfile::private_key(&mut std::io::BufReader::new(std::fs::File::open(&key)?))?
        .context("no private key in key.pem")?;
    let config = rustls::ServerConfig::builder_with_provider(Arc::new(rustls::crypto::ring::default_provider()))
        .with_safe_default_protocol_versions()?
        .with_no_client_auth()
        .with_single_cert(certs, key)?;
    Ok(TlsAcceptor::from(Arc::new(config)))
}
