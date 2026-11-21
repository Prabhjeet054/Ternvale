//! rustls server and client configuration (ring backend).

use std::path::Path;
use std::sync::Arc;

use rustls::pki_types::ServerName;
use rustls::server::WebPkiClientVerifier;
use rustls::{ClientConfig, RootCertStore, ServerConfig};

use crate::certs::{load_certs_and_key, PkiPaths};
use crate::error::GatewayError;

/// Install the ring crypto provider once per process.
#[tracing::instrument(level = "debug", target = "ternvale::gateway", skip_all)]
pub fn install_ring() {
    let _ = rustls::crypto::ring::default_provider().install_default();
}

/// Server TLS from `paths`. `require_mtls` rejects missing client certificates.
#[tracing::instrument(
    level = "debug",
    target = "ternvale::gateway",
    skip_all,
    fields(require_mtls)
)]
pub fn server_config(
    paths: &PkiPaths,
    require_mtls: bool,
) -> Result<Arc<ServerConfig>, GatewayError> {
    install_ring();
    let (certs, key) = load_certs_and_key(&paths.server_cert, &paths.server_key)?;
    let (ca, _) = load_certs_and_key(&paths.ca_cert, &paths.ca_key)?;
    let mut roots = RootCertStore::empty();
    for cert in ca {
        roots
            .add(cert)
            .map_err(|error| GatewayError::Tls(format!("ca: {error}")))?;
    }
    let verifier = WebPkiClientVerifier::builder(Arc::new(roots));
    let verifier = if require_mtls {
        verifier
            .build()
            .map_err(|error| GatewayError::Tls(format!("client verifier: {error}")))?
    } else {
        verifier
            .allow_unauthenticated()
            .build()
            .map_err(|error| GatewayError::Tls(format!("client verifier: {error}")))?
    };
    let mut config = ServerConfig::builder()
        .with_client_cert_verifier(verifier)
        .with_single_cert(certs, key)
        .map_err(|error| GatewayError::Tls(format!("server cert: {error}")))?;
    config.alpn_protocols = vec![b"http/1.1".to_vec()];
    tracing::info!(
        target: "ternvale::gateway",
        require_mtls,
        "tls server config ready"
    );
    Ok(Arc::new(config))
}

/// Client TLS that trusts `ca_pem`.
#[tracing::instrument(level = "debug", target = "ternvale::gateway", skip_all, fields(ca = %ca_path.display()))]
pub fn client_config(
    ca_path: &Path,
    client_cert: Option<&Path>,
    client_key: Option<&Path>,
) -> Result<Arc<ClientConfig>, GatewayError> {
    install_ring();
    let ca_bytes = std::fs::read(ca_path)
        .map_err(|source| GatewayError::io_path("read ca", ca_path.display(), source))?;
    let cas: Vec<_> = rustls_pemfile::certs(&mut ca_bytes.as_slice())
        .collect::<Result<Vec<_>, _>>()
        .map_err(|error| GatewayError::Tls(format!("parse ca: {error}")))?;
    let mut roots = RootCertStore::empty();
    for cert in cas {
        roots
            .add(cert)
            .map_err(|error| GatewayError::Tls(format!("ca: {error}")))?;
    }
    let builder = ClientConfig::builder().with_root_certificates(roots);
    let mut config = match (client_cert, client_key) {
        (Some(cert), Some(key)) => {
            let (certs, key) = load_certs_and_key(cert, key)?;
            builder
                .with_client_auth_cert(certs, key)
                .map_err(|error| GatewayError::Tls(format!("client cert: {error}")))?
        }
        (None, None) => builder.with_no_client_auth(),
        _ => {
            return Err(GatewayError::Config(
                "tls client certificate and key must both be set".into(),
            ));
        }
    };
    config.alpn_protocols = vec![b"http/1.1".to_vec()];
    Ok(Arc::new(config))
}

/// SNI for `https://host/...`.
#[tracing::instrument(level = "debug", target = "ternvale::gateway", skip_all, fields(host))]
pub fn server_name(host: &str) -> Result<ServerName<'static>, GatewayError> {
    ServerName::try_from(host.to_string())
        .map_err(|_| GatewayError::Tls(format!("invalid TLS server name {host:?}")))
}
