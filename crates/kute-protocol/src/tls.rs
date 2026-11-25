//! Authenticated, encrypted transport for the exchange protocol: TLS 1.3
//! with a certificate on both sides (mutual TLS), through `rustls`.
//!
//! The protocol itself carries no credentials. `HELLO` has a `peer` name,
//! and that name is a claim. Who the other side is comes from here: the
//! SHA-256 of the certificate it proved possession of during the handshake.
//! [`Access`] maps that fingerprint to a [`Policy`]; a peer that is not
//! listed is refused before any protocol message is read, even when its
//! certificate chains to a trusted authority.
//!
//! ```text
//! let (stream, who) = tls::accept(&config, tcp)?;      // handshake done
//! let policy = access.policy_for(&who)?;               // or refused
//! let peer = Peer::new("plant-b", engine, policy);
//! Session::new(peer, Framed::new(stream, max_frame)).serve()?;
//! ```

use crate::Policy;
use kute::{KuteError, Result};
use rustls::pki_types::pem::PemObject;
use rustls::pki_types::{CertificateDer, PrivateKeyDer, ServerName};
use rustls::server::WebPkiClientVerifier;
use rustls::{ClientConfig, ClientConnection, RootCertStore, ServerConfig, ServerConnection, StreamOwned};
use sha2::{Digest, Sha256};
use std::collections::BTreeMap;
use std::net::TcpStream;
use std::path::Path;
use std::sync::Arc;

fn tls_error(what: &str, e: impl std::fmt::Display) -> KuteError {
    KuteError::Backend(format!("tls: {what}: {e}"))
}

/// Who the other side proved to be.
#[derive(Clone, Debug, PartialEq, Eq, PartialOrd, Ord)]
pub struct PeerIdentity {
    /// `sha256:` and the hex SHA-256 of the peer's own certificate (DER).
    pub fingerprint: String,
}

/// The fingerprint [`PeerIdentity`] carries, for a certificate in hand.
pub fn fingerprint(certificate: &CertificateDer<'_>) -> String {
    format!("sha256:{}", kute::hex(&Sha256::digest(certificate.as_ref())))
}

/// Which peers may connect, and what each may do. Nobody is listed at first.
#[derive(Clone, Debug, Default)]
pub struct Access {
    peers: BTreeMap<String, Policy>,
}

impl Access {
    pub fn new() -> Self {
        Self::default()
    }

    /// Let the holder of the certificate with this fingerprint connect,
    /// under `policy`.
    pub fn allow(&mut self, fingerprint: &str, policy: Policy) -> Result<()> {
        let hex = fingerprint.strip_prefix("sha256:").unwrap_or("");
        if hex.len() != 64 || !hex.bytes().all(|b| b.is_ascii_hexdigit() && !b.is_ascii_uppercase()) {
            return Err(KuteError::InvalidConfig(format!("{fingerprint:?} is not a certificate fingerprint (sha256: and 64 lowercase hex digits)")));
        }
        self.peers.insert(fingerprint.to_string(), policy);
        Ok(())
    }

    /// The policy for an authenticated peer, or the refusal.
    pub fn policy_for(&self, who: &PeerIdentity) -> Result<Policy> {
        self.peers
            .get(&who.fingerprint)
            .cloned()
            .ok_or_else(|| KuteError::Backend(format!("peer {} is authenticated but not authorized here", who.fingerprint)))
    }

    pub fn len(&self) -> usize {
        self.peers.len()
    }

    pub fn is_empty(&self) -> bool {
        self.peers.is_empty()
    }
}

/// Certificates from a PEM file, in order.
pub fn load_certificates(path: &Path) -> Result<Vec<CertificateDer<'static>>> {
    let certs = CertificateDer::pem_file_iter(path)
        .map_err(|e| tls_error(&format!("reading {}", path.display()), e))?
        .collect::<std::result::Result<Vec<_>, _>>()
        .map_err(|e| tls_error(&format!("reading {}", path.display()), e))?;
    if certs.is_empty() {
        return Err(KuteError::InvalidConfig(format!("{} holds no certificate", path.display())));
    }
    Ok(certs)
}

/// The private key in a PEM file.
pub fn load_private_key(path: &Path) -> Result<PrivateKeyDer<'static>> {
    PrivateKeyDer::from_pem_file(path).map_err(|e| tls_error(&format!("reading the key in {}", path.display()), e))
}

fn roots(authorities: Vec<CertificateDer<'static>>) -> Result<RootCertStore> {
    let mut store = RootCertStore::empty();
    for authority in authorities {
        store.add(authority).map_err(|e| tls_error("a trusted authority certificate is not usable", e))?;
    }
    if store.is_empty() {
        return Err(KuteError::InvalidConfig("no trusted authority was given".into()));
    }
    Ok(store)
}

fn provider() -> Arc<rustls::crypto::CryptoProvider> {
    Arc::new(rustls::crypto::ring::default_provider())
}

/// A listening side that requires a client certificate issued by one of
/// `client_authorities`. TLS 1.3 only.
pub fn server_config(
    chain: Vec<CertificateDer<'static>>,
    key: PrivateKeyDer<'static>,
    client_authorities: Vec<CertificateDer<'static>>,
) -> Result<Arc<ServerConfig>> {
    let verifier = WebPkiClientVerifier::builder_with_provider(Arc::new(roots(client_authorities)?), provider())
        .build()
        .map_err(|e| tls_error("client verification", e))?;
    let config = ServerConfig::builder_with_provider(provider())
        .with_protocol_versions(&[&rustls::version::TLS13])
        .map_err(|e| tls_error("protocol versions", e))?
        .with_client_cert_verifier(verifier)
        .with_single_cert(chain, key)
        .map_err(|e| tls_error("the server certificate and key", e))?;
    Ok(Arc::new(config))
}

/// A connecting side that presents `chain` and trusts servers issued by one
/// of `server_authorities`. TLS 1.3 only.
pub fn client_config(
    chain: Vec<CertificateDer<'static>>,
    key: PrivateKeyDer<'static>,
    server_authorities: Vec<CertificateDer<'static>>,
) -> Result<Arc<ClientConfig>> {
    let config = ClientConfig::builder_with_provider(provider())
        .with_protocol_versions(&[&rustls::version::TLS13])
        .map_err(|e| tls_error("protocol versions", e))?
        .with_root_certificates(roots(server_authorities)?)
        .with_client_auth_cert(chain, key)
        .map_err(|e| tls_error("the client certificate and key", e))?;
    Ok(Arc::new(config))
}

fn identity(certificates: Option<&[CertificateDer<'_>]>) -> Result<PeerIdentity> {
    let own = certificates.and_then(|c| c.first()).ok_or_else(|| KuteError::Backend("tls: the peer presented no certificate".into()))?;
    Ok(PeerIdentity { fingerprint: fingerprint(own) })
}

/// Listening side: complete the handshake on an accepted connection.
/// Returns the encrypted stream and who the client proved to be. Fails, and
/// nothing of the protocol is read, if the client has no certificate from a
/// trusted authority.
pub fn accept(config: &Arc<ServerConfig>, mut tcp: TcpStream) -> Result<(StreamOwned<ServerConnection, TcpStream>, PeerIdentity)> {
    let mut connection = ServerConnection::new(config.clone()).map_err(|e| tls_error("starting the handshake", e))?;
    while connection.is_handshaking() {
        connection.complete_io(&mut tcp).map_err(|e| tls_error("handshake", e))?;
    }
    let who = identity(connection.peer_certificates())?;
    Ok((StreamOwned::new(connection, tcp), who))
}

/// Connecting side: complete the handshake with the server named
/// `server_name` (it must be in the server's certificate). Returns the
/// encrypted stream and the server's identity, for a caller that wants to
/// accept one particular server and not any the authority issued.
pub fn connect(config: &Arc<ClientConfig>, server_name: &str, mut tcp: TcpStream) -> Result<(StreamOwned<ClientConnection, TcpStream>, PeerIdentity)> {
    let name = ServerName::try_from(server_name.to_string()).map_err(|e| tls_error(&format!("server name {server_name:?}"), e))?;
    let mut connection = ClientConnection::new(config.clone(), name).map_err(|e| tls_error("starting the handshake", e))?;
    while connection.is_handshaking() {
        connection.complete_io(&mut tcp).map_err(|e| tls_error("handshake", e))?;
    }
    let who = identity(connection.peer_certificates())?;
    Ok((StreamOwned::new(connection, tcp), who))
}
