//! Self-signed CA plus server/client certificates. Rotation is documented in
//! `docs/GATEWAY.md`.

use std::fs;
use std::net::{IpAddr, Ipv4Addr, Ipv6Addr};
use std::os::unix::fs::PermissionsExt;
use std::path::{Path, PathBuf};

use rcgen::{
    BasicConstraints, CertificateParams, DistinguishedName, DnType, IsCa, KeyPair, KeyUsagePurpose,
    SanType,
};
use rustls_pki_types::{CertificateDer, PrivateKeyDer};

use crate::error::GatewayError;

/// Files written by [`generate_pki`].
#[derive(Debug, Clone)]
pub struct PkiPaths {
    /// CA certificate PEM.
    pub ca_cert: PathBuf,
    /// CA private key PEM (0600).
    pub ca_key: PathBuf,
    /// Server certificate PEM.
    pub server_cert: PathBuf,
    /// Server private key PEM (0600).
    pub server_key: PathBuf,
    /// Sample client certificate PEM (mTLS).
    pub client_cert: PathBuf,
    /// Sample client private key PEM (0600).
    pub client_key: PathBuf,
}

impl PkiPaths {
    /// Standard layout under `dir`.
    #[tracing::instrument(level = "debug", target = "ternvale::gateway", skip_all, fields(dir = %dir.display()))]
    pub fn in_dir(dir: &Path) -> Self {
        Self {
            ca_cert: dir.join("ca.pem"),
            ca_key: dir.join("ca.key"),
            server_cert: dir.join("server.pem"),
            server_key: dir.join("server.key"),
            client_cert: dir.join("client.pem"),
            client_key: dir.join("client.key"),
        }
    }
}

/// Generate a new CA and leaf certs. Overwrites existing files.
#[tracing::instrument(level = "debug", target = "ternvale::gateway", skip_all, fields(dir = %dir.display()))]
pub fn generate_pki(dir: &Path) -> Result<PkiPaths, GatewayError> {
    fs::create_dir_all(dir)
        .map_err(|source| GatewayError::io_path("create cert directory", dir.display(), source))?;
    fs::set_permissions(dir, fs::Permissions::from_mode(0o700))
        .map_err(|source| GatewayError::io_path("chmod cert directory", dir.display(), source))?;

    let ca_key = KeyPair::generate().map_err(|error| GatewayError::Cert(error.to_string()))?;
    let mut ca_params = CertificateParams::new(Vec::new())
        .map_err(|error| GatewayError::Cert(error.to_string()))?;
    ca_params.is_ca = IsCa::Ca(BasicConstraints::Unconstrained);
    ca_params.key_usages = vec![
        KeyUsagePurpose::KeyCertSign,
        KeyUsagePurpose::CrlSign,
        KeyUsagePurpose::DigitalSignature,
    ];
    let mut ca_dn = DistinguishedName::new();
    ca_dn.push(DnType::CommonName, "Ternvale Gateway CA");
    ca_params.distinguished_name = ca_dn;
    let ca_cert = ca_params
        .self_signed(&ca_key)
        .map_err(|error| GatewayError::Cert(error.to_string()))?;
    let ca_pem = ca_cert.pem();

    let server_key = KeyPair::generate().map_err(|error| GatewayError::Cert(error.to_string()))?;
    let server_cert = signed_leaf(
        "ternvaled",
        vec![
            SanType::DnsName(
                "localhost"
                    .try_into()
                    .map_err(|error| GatewayError::Cert(format!("{error}")))?,
            ),
            SanType::IpAddress(IpAddr::V4(Ipv4Addr::LOCALHOST)),
            SanType::IpAddress(IpAddr::V6(Ipv6Addr::LOCALHOST)),
        ],
        &server_key,
        &ca_cert,
        &ca_key,
    )?;

    let client_key = KeyPair::generate().map_err(|error| GatewayError::Cert(error.to_string()))?;
    let client_cert = signed_leaf("ternvale-cli", Vec::new(), &client_key, &ca_cert, &ca_key)?;

    let paths = PkiPaths::in_dir(dir);
    write_pem(&paths.ca_cert, ca_pem.as_bytes(), 0o644)?;
    write_pem(&paths.ca_key, ca_key.serialize_pem().as_bytes(), 0o600)?;
    write_pem(&paths.server_cert, server_cert.pem().as_bytes(), 0o644)?;
    write_pem(
        &paths.server_key,
        server_key.serialize_pem().as_bytes(),
        0o600,
    )?;
    write_pem(&paths.client_cert, client_cert.pem().as_bytes(), 0o644)?;
    write_pem(
        &paths.client_key,
        client_key.serialize_pem().as_bytes(),
        0o600,
    )?;
    tracing::info!(
        target: "ternvale::gateway",
        dir = %dir.display(),
        "generated gateway CA and leaf certificates"
    );
    Ok(paths)
}

/// Load PEM certs and a PKCS#8 key from disk.
#[tracing::instrument(level = "debug", target = "ternvale::gateway", skip_all, fields(cert = %cert_path.display()))]
pub fn load_certs_and_key(
    cert_path: &Path,
    key_path: &Path,
) -> Result<(Vec<CertificateDer<'static>>, PrivateKeyDer<'static>), GatewayError> {
    let cert_pem = fs::read(cert_path)
        .map_err(|source| GatewayError::io_path("read certificate", cert_path.display(), source))?;
    let key_pem = fs::read(key_path)
        .map_err(|source| GatewayError::io_path("read private key", key_path.display(), source))?;
    let certs: Vec<CertificateDer<'static>> = rustls_pemfile::certs(&mut cert_pem.as_slice())
        .collect::<Result<Vec<_>, _>>()
        .map_err(|error| GatewayError::Tls(format!("parse certificate: {error}")))?;
    if certs.is_empty() {
        return Err(GatewayError::Tls(format!(
            "no certificates in {}",
            cert_path.display()
        )));
    }
    let mut key_slice = key_pem.as_slice();
    let mut keys = rustls_pemfile::pkcs8_private_keys(&mut key_slice);
    let key = keys
        .next()
        .ok_or_else(|| GatewayError::Tls(format!("no PKCS#8 key in {}", key_path.display())))?
        .map_err(|error| GatewayError::Tls(format!("parse private key: {error}")))?;
    Ok((certs, PrivateKeyDer::Pkcs8(key)))
}

#[tracing::instrument(level = "debug", target = "ternvale::gateway", skip_all)]
fn signed_leaf(
    cn: &str,
    sans: Vec<SanType>,
    key: &KeyPair,
    issuer: &rcgen::Certificate,
    issuer_key: &KeyPair,
) -> Result<rcgen::Certificate, GatewayError> {
    let mut params = CertificateParams::new(Vec::new())
        .map_err(|error| GatewayError::Cert(error.to_string()))?;
    params.key_usages = vec![
        KeyUsagePurpose::DigitalSignature,
        KeyUsagePurpose::KeyEncipherment,
    ];
    params.subject_alt_names = sans;
    let mut dn = DistinguishedName::new();
    dn.push(DnType::CommonName, cn);
    params.distinguished_name = dn;
    params
        .signed_by(key, issuer, issuer_key)
        .map_err(|error| GatewayError::Cert(error.to_string()))
}

#[tracing::instrument(level = "debug", target = "ternvale::gateway", skip_all, fields(path = %path.display()))]
fn write_pem(path: &Path, bytes: &[u8], mode: u32) -> Result<(), GatewayError> {
    fs::write(path, bytes)
        .map_err(|source| GatewayError::io_path("write pem", path.display(), source))?;
    fs::set_permissions(path, fs::Permissions::from_mode(mode))
        .map_err(|source| GatewayError::io_path("chmod pem", path.display(), source))?;
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn generate_writes_pem() {
        let dir = std::env::temp_dir().join(format!("tvpki-{}", std::process::id()));
        let _ = fs::remove_dir_all(&dir);
        let paths = generate_pki(&dir).expect("pki");
        assert!(paths.ca_cert.is_file());
        assert!(paths.server_cert.is_file());
        load_certs_and_key(&paths.server_cert, &paths.server_key).expect("load");
        let _ = fs::remove_dir_all(&dir);
    }
}
