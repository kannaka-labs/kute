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
pub use rustls::{ClientConfig, ServerConfig};
use rustls::{ClientConnection, ConnectionCommon, RootCertStore, ServerConnection, SideData, StreamOwned};
use sha2::{Digest, Sha256};
use std::collections::BTreeMap;
use std::io::{Read, Write};
use std::net::TcpStream;
use std::ops::DerefMut;
use std::path::Path;
use std::sync::Arc;
use std::time::{Duration, Instant};

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

/// An encrypted connection. Dropping it tells the other side that the
/// stream ends here (TLS `close_notify`), so the other side can tell a
/// finished conversation from one that was cut: a connection that ends
/// without that notice is reported as an error by whoever is reading it.
pub struct Secured<C, S>(StreamOwned<C, TcpStream>)
where
    C: DerefMut<Target = ConnectionCommon<S>>,
    S: SideData;

/// The listening side's connection.
pub type ServerStream = Secured<ServerConnection, rustls::server::ServerConnectionData>;
/// The connecting side's connection.
pub type ClientStream = Secured<ClientConnection, rustls::client::ClientConnectionData>;

impl<C: DerefMut<Target = ConnectionCommon<S>>, S: SideData> Secured<C, S> {
    /// The socket under the encryption, for setting how long a read or a
    /// write may take. Reading or writing it directly would break the
    /// connection.
    pub fn socket(&self) -> &TcpStream {
        &self.0.sock
    }

    /// Tell the other side that nothing more will be sent, and send what
    /// is waiting. Dropping does the same; this is for a side that wants
    /// to go on reading afterwards.
    pub fn close(&mut self) {
        self.0.conn.send_close_notify();
        let _ = self.0.flush();
    }
}

impl<C: DerefMut<Target = ConnectionCommon<S>>, S: SideData> Read for Secured<C, S> {
    fn read(&mut self, buf: &mut [u8]) -> std::io::Result<usize> {
        self.0.read(buf)
    }
}

impl<C: DerefMut<Target = ConnectionCommon<S>>, S: SideData> Write for Secured<C, S> {
    fn write(&mut self, buf: &[u8]) -> std::io::Result<usize> {
        self.0.write(buf)
    }
    fn flush(&mut self) -> std::io::Result<()> {
        self.0.flush()
    }
}

impl<C: DerefMut<Target = ConnectionCommon<S>>, S: SideData> Drop for Secured<C, S> {
    fn drop(&mut self) {
        self.0.conn.send_close_notify();
        let _ = self.0.flush();
    }
}

/// The handshake, with one deadline for all of it. Each read and write
/// is given what is left of the time, and the time is looked at again
/// after each: a side that sends a byte now and then cannot hold the
/// other past the deadline.
fn handshake<C: DerefMut<Target = ConnectionCommon<S>>, S: SideData>(connection: &mut C, tcp: &mut TcpStream, deadline: Instant) -> Result<()> {
    let late = || tls_error("handshake", "it did not finish in the time allowed");
    loop {
        let left = deadline.checked_duration_since(Instant::now()).filter(|d| !d.is_zero()).ok_or_else(late)?;
        tcp.set_read_timeout(Some(left)).map_err(|e| tls_error("handshake", e))?;
        tcp.set_write_timeout(Some(left)).map_err(|e| tls_error("handshake", e))?;
        if connection.wants_write() {
            connection.write_tls(tcp).map_err(|e| tls_error("handshake", e))?;
            continue;
        }
        if !connection.is_handshaking() {
            return Ok(());
        }
        match connection.read_tls(tcp) {
            Ok(0) => return Err(tls_error("handshake", "the connection closed before it finished")),
            Ok(_) => {}
            Err(e) => return Err(tls_error("handshake", e)),
        }
        if let Err(e) = connection.process_new_packets() {
            // Say why, if there is time; the other side may be honest
            // and misconfigured.
            let _ = connection.write_tls(tcp);
            said_why(tcp, deadline);
            return Err(tls_error("handshake", e));
        }
    }
}

/// How long, at most, a refused side is read off so that the reason
/// reaches it.
const LINGER: Duration = Duration::from_millis(250);
/// And how much of what it sent is read off in that time.
const LINGER_BYTES: usize = 64 * 1024;

/// After the alert that says why: end this side's sending, and read off
/// a little of what the other side already sent. A connection closed
/// with bytes unread is reset, and a reset can throw away the alert
/// before the other side has read it (in TLS 1.3 a client sends its
/// request before it knows its certificate was refused). Bounded by
/// time, by bytes and by the handshake's own deadline, so a refused
/// side holds this one no longer than a silent one could.
fn said_why(tcp: &mut TcpStream, deadline: Instant) {
    let _ = tcp.shutdown(std::net::Shutdown::Write);
    let until = deadline.min(Instant::now() + LINGER);
    let mut scratch = [0u8; 4096];
    let mut seen = 0usize;
    while seen < LINGER_BYTES {
        let Some(left) = until.checked_duration_since(Instant::now()).filter(|d| !d.is_zero()) else { return };
        if tcp.set_read_timeout(Some(left)).is_err() {
            return;
        }
        match tcp.read(&mut scratch) {
            Ok(0) | Err(_) => return,
            Ok(n) => seen += n,
        }
    }
}

/// As [`accept`], and the handshake must finish by `deadline`.
pub fn accept_by(config: &Arc<ServerConfig>, mut tcp: TcpStream, deadline: Instant) -> Result<(ServerStream, PeerIdentity)> {
    let mut connection = ServerConnection::new(config.clone()).map_err(|e| tls_error("starting the handshake", e))?;
    handshake(&mut connection, &mut tcp, deadline)?;
    let who = identity(connection.peer_certificates())?;
    Ok((Secured(StreamOwned::new(connection, tcp)), who))
}

/// As [`connect`], and the handshake must finish by `deadline`.
pub fn connect_by(config: &Arc<ClientConfig>, server_name: &str, mut tcp: TcpStream, deadline: Instant) -> Result<(ClientStream, PeerIdentity)> {
    let name = ServerName::try_from(server_name.to_string()).map_err(|e| tls_error(&format!("server name {server_name:?}"), e))?;
    let mut connection = ClientConnection::new(config.clone(), name).map_err(|e| tls_error("starting the handshake", e))?;
    handshake(&mut connection, &mut tcp, deadline)?;
    let who = identity(connection.peer_certificates())?;
    Ok((Secured(StreamOwned::new(connection, tcp)), who))
}

fn identity(certificates: Option<&[CertificateDer<'_>]>) -> Result<PeerIdentity> {
    let own = certificates.and_then(|c| c.first()).ok_or_else(|| KuteError::Backend("tls: the peer presented no certificate".into()))?;
    Ok(PeerIdentity { fingerprint: fingerprint(own) })
}

/// Listening side: complete the handshake on an accepted connection.
/// Returns the encrypted stream and who the client proved to be. Fails, and
/// nothing of the protocol is read, if the client has no certificate from a
/// trusted authority.
pub fn accept(config: &Arc<ServerConfig>, mut tcp: TcpStream) -> Result<(ServerStream, PeerIdentity)> {
    let mut connection = ServerConnection::new(config.clone()).map_err(|e| tls_error("starting the handshake", e))?;
    while connection.is_handshaking() {
        connection.complete_io(&mut tcp).map_err(|e| tls_error("handshake", e))?;
    }
    let who = identity(connection.peer_certificates())?;
    Ok((Secured(StreamOwned::new(connection, tcp)), who))
}

/// Connecting side: complete the handshake with the server named
/// `server_name` (it must be in the server's certificate). Returns the
/// encrypted stream and the server's identity, for a caller that wants to
/// accept one particular server and not any the authority issued.
pub fn connect(config: &Arc<ClientConfig>, server_name: &str, mut tcp: TcpStream) -> Result<(ClientStream, PeerIdentity)> {
    let name = ServerName::try_from(server_name.to_string()).map_err(|e| tls_error(&format!("server name {server_name:?}"), e))?;
    let mut connection = ClientConnection::new(config.clone(), name).map_err(|e| tls_error("starting the handshake", e))?;
    while connection.is_handshaking() {
        connection.complete_io(&mut tcp).map_err(|e| tls_error("handshake", e))?;
    }
    let who = identity(connection.peer_certificates())?;
    Ok((Secured(StreamOwned::new(connection, tcp)), who))
}
