// TODO: this is really messy clean it up

use std::path::{Path, PathBuf};

use anyhow::Result;

use crate::ALPN_PROTOCOL;

pub enum AuthConfig {
    /// Emit self signed certs, for development
    DebugConfig,
    /// Reads certs from a pem file
    ProdConfig {
        certs_pem_file: PathBuf,
        key_pem_file: PathBuf,
    },
}

pub fn build_tls_config(config: &AuthConfig) -> Result<rustls::ServerConfig> {
    let (certs, key) = match config {
        AuthConfig::DebugConfig => {
            let cert = rcgen::generate_simple_self_signed(vec!["localhost".into()])?;
            let cert_der = rustls::pki_types::CertificateDer::from(cert.cert);
            let key_der =
                rustls::pki_types::PrivateKeyDer::try_from(cert.signing_key.serialize_der())
                    .map_err(|e| anyhow::anyhow!("failed to parse generated key: {}", e))?;

            (vec![cert_der], key_der)
        }
        AuthConfig::ProdConfig {
            certs_pem_file,
            key_pem_file,
        } => (
            load_certs_from_pem(&certs_pem_file)?,
            load_key_from_pem(&key_pem_file)?,
        ),
    };

    let mut tls_config = rustls::ServerConfig::builder()
        .with_no_client_auth()
        .with_single_cert(certs, key)?;
    tls_config.alpn_protocols = vec![ALPN_PROTOCOL.to_vec()];

    Ok(tls_config)
}

fn load_certs_from_pem(path: &Path) -> Result<Vec<rustls::pki_types::CertificateDer<'static>>> {
    let file = std::fs::File::open(path)?;
    let mut reader = std::io::BufReader::new(file);
    let certs: Vec<_> =
        rustls_pemfile::certs(&mut reader).collect::<std::result::Result<_, _>>()?;
    Ok(certs)
}

fn load_key_from_pem(path: &Path) -> Result<rustls::pki_types::PrivateKeyDer<'static>> {
    let file = std::fs::File::open(path)?;
    let mut reader = std::io::BufReader::new(file);
    let key = rustls_pemfile::private_key(&mut reader)?
        .ok_or_else(|| anyhow::anyhow!("no private key found in {:?}", path))?;
    Ok(key)
}
