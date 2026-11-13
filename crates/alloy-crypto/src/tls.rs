//! TLS 1.3 for the API endpoints (ARFC 0008 clauses 3.8 and 3.8.1), on
//! `rustls` with the `aws-lc-rs` provider (ADR 0053).
//!
//! Every configuration here is TLS 1.3 only, uses only the key exchange
//! groups `agent/eng/crypto.md` rule 4 lists, and takes wall time from the
//! caller's [`WallClock`]. Session tickets, resumption, early data, and
//! Encrypted Client Hello are off on both sides, so the provider draws
//! randomness only for the values of one handshake (ADR 0042).
//!
//! No Alloy key is given to the provider. A server presents a [`ServerCert`],
//! whose key is an [`ecdsa::SigningKey`](crate::ecdsa::SigningKey) that signs
//! the `CertificateVerify` with the RFC 6979 signer of [`crate::ecdsa`], so
//! the provider only verifies P-256.
//!
//! The server side reads the ClientHello first, with [`HelloReader`], so the
//! caller can choose a configuration by the client's ALPN protocols before
//! the handshake ([`ClientHello::handshake`]). The client side verifies the
//! server chain to a pinned API CA ([`ClientTls::pinned`]). Both give a
//! stream that reads and writes plaintext through `tokio`'s I/O traits.

use std::fmt;
use std::io;
use std::net::IpAddr;
use std::pin::Pin;
use std::sync::Arc;
use std::task::{Context, Poll};
use std::time::Duration;

use rustls::client::danger::{HandshakeSignatureValid, ServerCertVerified, ServerCertVerifier};
use rustls::client::{Resumption, WebPkiServerVerifier};
use rustls::crypto::{CryptoProvider, aws_lc_rs};
use rustls::pki_types::{CertificateDer, ServerName, SubjectPublicKeyInfoDer, UnixTime};
use rustls::server::{Accepted, Acceptor, NoServerSessionStorage};
use rustls::sign::{CertifiedKey, SingleCertAndKey};
use rustls::time_provider::TimeProvider;
use rustls::{
    CertificateError, ClientConfig, DigitallySignedStruct, RootCertStore, ServerConfig,
    SignatureAlgorithm, SignatureScheme,
};
use tokio::io::{AsyncRead, AsyncWrite, ReadBuf};

use crate::ecdsa::SigningKey;

/// The ALPN protocol id of the API (RFC 7301, RFC 9113).
pub const ALPN_H2: &[u8] = b"h2";

/// The ALPN protocol id of the `stream` underlay (ARFC 0004 section 3.6).
pub const ALPN_STREAM: &[u8] = b"alloy-al/1";

/// The TLS alert description `no_application_protocol` (RFC 7301 section
/// 3.2).
pub const NO_APPLICATION_PROTOCOL: u8 = 120;

/// The bytes of a fatal TLS alert `no_application_protocol` (RFC 7301
/// section 3.2, RFC 8446 sections 5.1 and 6): one plaintext record of type
/// alert (21), legacy version 0x0303, length 2, level fatal (2), description
/// 120.
pub const NO_APPLICATION_PROTOCOL_ALERT: [u8; 7] = [21, 3, 3, 0, 2, 2, NO_APPLICATION_PROTOCOL];

/// Wall time for certificate validity, which the caller takes from the
/// `alloy-rt` clock.
pub trait WallClock: Send + Sync + fmt::Debug {
    /// Whole seconds since 1970-01-01T00:00:00Z, or `None` when the clock
    /// reads before that.
    fn unix_seconds(&self) -> Option<u64>;
}

/// Why a TLS configuration, a certificate, or a handshake was refused.
#[derive(Debug, thiserror::Error)]
#[error("{0}")]
pub struct TlsError(rustls::Error);

/// Why a client handshake failed.
#[derive(Debug, thiserror::Error)]
pub enum HandshakeError {
    /// The server's certificate chain does not verify to the pinned API CA,
    /// or does not name the address connected to.
    #[error("the server's certificate was refused: {0}")]
    Certificate(TlsError),
    /// The server sent the fatal alert with this description (RFC 8446
    /// section 6), for example [`NO_APPLICATION_PROTOCOL`].
    #[error("the server sent the TLS alert {0}")]
    AlertReceived(u8),
    /// The handshake failed for another reason, I/O included.
    #[error("the TLS handshake failed: {0}")]
    Other(io::Error),
}

impl HandshakeError {
    fn of(error: io::Error) -> HandshakeError {
        let tls = error
            .get_ref()
            .and_then(|e| e.downcast_ref::<rustls::Error>())
            .cloned();
        match tls {
            Some(e @ rustls::Error::InvalidCertificate(_)) => {
                HandshakeError::Certificate(TlsError(e))
            }
            Some(rustls::Error::AlertReceived(alert)) => {
                HandshakeError::AlertReceived(u8::from(alert))
            }
            _ => HandshakeError::Other(error),
        }
    }
}

/// The `aws-lc-rs` provider with the key exchange groups of
/// `agent/eng/crypto.md` rule 4: X25519MLKEM768, X25519, and secp256r1.
fn provider() -> CryptoProvider {
    let mut provider = aws_lc_rs::default_provider();
    provider.kx_groups = vec![
        aws_lc_rs::kx_group::X25519MLKEM768,
        aws_lc_rs::kx_group::X25519,
        aws_lc_rs::kx_group::SECP256R1,
    ];
    provider
}

/// A certificate chain and its P-256 key, as a server presents them.
/// Clones share them.
#[derive(Clone)]
pub struct ServerCert(Arc<CertifiedKey>);

impl fmt::Debug for ServerCert {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("ServerCert").finish_non_exhaustive()
    }
}

impl ServerCert {
    /// The chain `chain`, DER certificates leaf first, presented with `key`,
    /// which signs each `CertificateVerify` with `ecdsa_secp256r1_sha256`
    /// (RFC 8446 section 4.2.3) and an RFC 6979 nonce.
    ///
    /// # Errors
    ///
    /// [`TlsError`] when the chain is empty, the leaf does not parse, or the
    /// leaf's key is not `key`'s public key.
    pub fn new(chain: Vec<Vec<u8>>, key: SigningKey) -> Result<ServerCert, TlsError> {
        let spki = key
            .public_key()
            .spki_der()
            .map_err(|e| TlsError(rustls::Error::General(e.to_string())))?;
        let key = Arc::new(P256Key {
            key: Arc::new(key),
            spki,
        });
        let chain = chain.into_iter().map(CertificateDer::from).collect();
        let certified = CertifiedKey::new(chain, key);
        certified.keys_match().map_err(TlsError)?;
        Ok(ServerCert(Arc::new(certified)))
    }
}

/// An Alloy P-256 key as `rustls` takes a server key: it signs only with
/// `ecdsa_secp256r1_sha256`, through the RFC 6979 signer.
struct P256Key {
    key: Arc<SigningKey>,
    spki: Vec<u8>,
}

impl fmt::Debug for P256Key {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("P256Key").finish_non_exhaustive()
    }
}

impl rustls::sign::SigningKey for P256Key {
    fn choose_scheme(&self, offered: &[SignatureScheme]) -> Option<Box<dyn rustls::sign::Signer>> {
        offered
            .contains(&SignatureScheme::ECDSA_NISTP256_SHA256)
            .then(|| Box::new(P256Signer(Arc::clone(&self.key))) as Box<dyn rustls::sign::Signer>)
    }

    fn public_key(&self) -> Option<SubjectPublicKeyInfoDer<'_>> {
        Some(SubjectPublicKeyInfoDer::from(self.spki.as_slice()))
    }

    fn algorithm(&self) -> SignatureAlgorithm {
        SignatureAlgorithm::ECDSA
    }
}

/// One `CertificateVerify` signature.
struct P256Signer(Arc<SigningKey>);

impl fmt::Debug for P256Signer {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("P256Signer").finish_non_exhaustive()
    }
}

impl rustls::sign::Signer for P256Signer {
    fn sign(&self, message: &[u8]) -> Result<Vec<u8>, rustls::Error> {
        // TLS 1.3 carries an ECDSA signature as the DER `ECDSA-Sig-Value`
        // (RFC 8446 section 4.2.3).
        Ok(self.0.sign_der(message))
    }

    fn scheme(&self) -> SignatureScheme {
        SignatureScheme::ECDSA_NISTP256_SHA256
    }
}

/// The server configuration of one ALPN protocol. Clones share it.
#[derive(Clone)]
pub struct ServerTls(Arc<ServerConfig>);

impl fmt::Debug for ServerTls {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("ServerTls").finish_non_exhaustive()
    }
}

impl ServerTls {
    /// TLS 1.3 only, the ALPN protocol id `alpn` only, `cert` presented, no
    /// client certificate, no session ticket, no resumption state, and no
    /// early data.
    ///
    /// # Errors
    ///
    /// [`TlsError`] when `rustls` refuses the configuration.
    pub fn new(
        clock: Arc<dyn WallClock>,
        cert: &ServerCert,
        alpn: &[u8],
    ) -> Result<ServerTls, TlsError> {
        let mut config =
            ServerConfig::builder_with_details(Arc::new(provider()), Arc::new(RtTime(clock)))
                .with_protocol_versions(&[&rustls::version::TLS13])
                .map_err(TlsError)?
                .with_no_client_auth()
                .with_cert_resolver(Arc::new(SingleCertAndKey::from(Arc::clone(&cert.0))));
        config.alpn_protocols = vec![alpn.to_vec()];
        config.session_storage = Arc::new(NoServerSessionStorage {});
        config.send_tls13_tickets = 0;
        config.max_early_data_size = 0;
        config.send_half_rtt_data = false;
        Ok(ServerTls(Arc::new(config)))
    }
}

/// The client configuration of an API client. Clones share it.
#[derive(Clone)]
pub struct ClientTls(Arc<ClientConfig>);

impl fmt::Debug for ClientTls {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("ClientTls").finish_non_exhaustive()
    }
}

impl ClientTls {
    /// TLS 1.3 only, offering the ALPN protocol ids `alpn` in that order, no
    /// resumption, no early data, no Encrypted Client Hello, and a server
    /// chain that must verify to a CA certificate it presents whose
    /// SubjectPublicKeyInfo has the fingerprint `api_ca_fp`, and must name
    /// the address connected to among the leaf's `iPAddress` names (ARFC
    /// 0008 clause 3.8, ARFC 0002 section 3.7).
    ///
    /// # Errors
    ///
    /// [`TlsError`] when `rustls` refuses the configuration.
    pub fn pinned(
        clock: Arc<dyn WallClock>,
        api_ca_fp: ApiCaFingerprint,
        alpn: &[&[u8]],
    ) -> Result<ClientTls, TlsError> {
        let provider = Arc::new(provider());
        let verifier = PinnedCa {
            pin: api_ca_fp,
            provider: Arc::clone(&provider),
        };
        let mut config =
            ClientConfig::builder_with_details(Arc::clone(&provider), Arc::new(RtTime(clock)))
                .with_protocol_versions(&[&rustls::version::TLS13])
                .map_err(TlsError)?
                .dangerous()
                .with_custom_certificate_verifier(Arc::new(verifier))
                .with_no_client_auth();
        config.alpn_protocols = alpn.iter().map(|id| id.to_vec()).collect();
        config.resumption = Resumption::disabled();
        config.enable_early_data = false;
        Ok(ClientTls(Arc::new(config)))
    }

    /// Runs the client handshake over `io` to the server at `server`.
    ///
    /// # Errors
    ///
    /// [`HandshakeError::Certificate`] for a chain that fails the pin or the
    /// address, [`HandshakeError::AlertReceived`] when the server sends an
    /// alert, and [`HandshakeError::Other`] otherwise.
    pub async fn connect<IO>(
        &self,
        server: IpAddr,
        io: IO,
    ) -> Result<ClientStream<IO>, HandshakeError>
    where
        IO: AsyncRead + AsyncWrite + Unpin,
    {
        tokio_rustls::TlsConnector::from(Arc::clone(&self.0))
            .connect(ServerName::IpAddress(server.into()), io)
            .await
            .map(|tls| ClientStream(Box::new(tls)))
            .map_err(HandshakeError::of)
    }
}

/// `api_ca_fp` (ARFC 0002 section 3.7): the first 16 bytes of the BLAKE3 hash
/// of the DER encoding of the API CA certificate's SubjectPublicKeyInfo.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub struct ApiCaFingerprint(pub [u8; 16]);

impl ApiCaFingerprint {
    /// The fingerprint of a DER certificate's SubjectPublicKeyInfo, or `None`
    /// when the certificate does not parse.
    #[must_use]
    pub fn of_certificate(cert: &[u8]) -> Option<ApiCaFingerprint> {
        let cert = CertificateDer::from(cert);
        let anchor = webpki::anchor_from_trusted_cert(&cert).ok()?;
        let spki = webpki::spki_for_anchor(&anchor);
        Some(ApiCaFingerprint::of_spki(spki.as_ref()))
    }

    /// The fingerprint of a DER-encoded SubjectPublicKeyInfo.
    #[must_use]
    pub fn of_spki(spki_der: &[u8]) -> ApiCaFingerprint {
        let mut fp = [0; 16];
        fp.copy_from_slice(&crate::hash(spki_der)[..16]);
        ApiCaFingerprint(fp)
    }
}

/// Reads a ClientHello from the first bytes of a server connection, before a
/// configuration is chosen. It buffers at most what `rustls` buffers for one
/// handshake message.
#[derive(Default)]
pub struct HelloReader(Acceptor);

impl fmt::Debug for HelloReader {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("HelloReader").finish_non_exhaustive()
    }
}

/// What a [`HelloReader`] has after the bytes given so far.
#[derive(Debug)]
pub enum Hello {
    /// Not yet a whole ClientHello: give it more bytes.
    Incomplete,
    /// A whole ClientHello.
    Complete(ClientHello),
    /// Not a ClientHello. Send `alert`, which may be empty, then close.
    Refused {
        /// The TLS alert record to send.
        alert: Vec<u8>,
    },
}

impl HelloReader {
    /// A reader that has seen no byte.
    #[must_use]
    pub fn new() -> HelloReader {
        HelloReader::default()
    }

    /// Takes the next bytes the client sent, all of `bytes`, and says what
    /// they make. After [`Hello::Complete`] or [`Hello::Refused`] the reader
    /// is spent and refuses every further byte.
    pub fn feed(&mut self, mut bytes: &[u8]) -> Hello {
        while !bytes.is_empty() {
            match self.0.read_tls(&mut bytes) {
                Ok(0) | Err(_) => return Hello::Refused { alert: Vec::new() },
                Ok(_) => {}
            }
        }
        match self.0.accept() {
            Ok(Some(accepted)) => Hello::Complete(ClientHello(Box::new(accepted))),
            Ok(None) => Hello::Incomplete,
            Err((_, mut alert)) => {
                let mut bytes = Vec::new();
                if alert.write_all(&mut bytes).is_err() {
                    bytes.clear();
                }
                Hello::Refused { alert: bytes }
            }
        }
    }
}

/// A whole ClientHello, before the server has answered it.
pub struct ClientHello(Box<Accepted>);

impl fmt::Debug for ClientHello {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("ClientHello").finish_non_exhaustive()
    }
}

impl ClientHello {
    /// The ALPN protocol ids the client offers, in its order, or `None`
    /// when the ClientHello has no ALPN extension.
    #[must_use]
    pub fn alpn(&self) -> Option<Vec<&[u8]>> {
        self.0.client_hello().alpn().map(Iterator::collect)
    }

    /// Completes the handshake over `io` with `config`. `io` gives the bytes
    /// that follow the ClientHello; those already given to the
    /// [`HelloReader`] are not read again.
    ///
    /// # Errors
    ///
    /// The I/O or TLS error that ended the handshake.
    pub async fn handshake<IO>(self, io: IO, config: &ServerTls) -> io::Result<ServerStream<IO>>
    where
        IO: AsyncRead + AsyncWrite + Unpin,
    {
        tokio_rustls::StartHandshake::from_parts(*self.0, io)
            .into_stream(Arc::clone(&config.0))
            .await
            .map(|tls| ServerStream(Box::new(tls)))
    }
}

/// A server connection after its handshake: plaintext in and out.
pub struct ServerStream<IO>(Box<tokio_rustls::server::TlsStream<IO>>);

impl<IO> fmt::Debug for ServerStream<IO> {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("ServerStream").finish_non_exhaustive()
    }
}

/// A client connection after its handshake: plaintext in and out.
pub struct ClientStream<IO>(Box<tokio_rustls::client::TlsStream<IO>>);

impl<IO> fmt::Debug for ClientStream<IO> {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("ClientStream").finish_non_exhaustive()
    }
}

impl<IO> ClientStream<IO> {
    /// The ALPN protocol id the server chose, if any.
    #[must_use]
    pub fn alpn(&self) -> Option<&[u8]> {
        self.0.get_ref().1.alpn_protocol()
    }
}

/// Delegates `tokio`'s I/O traits to the `tokio-rustls` stream inside.
macro_rules! delegate_io {
    ($type:ident) => {
        impl<IO: AsyncRead + AsyncWrite + Unpin> AsyncRead for $type<IO> {
            fn poll_read(
                self: Pin<&mut Self>,
                cx: &mut Context<'_>,
                buf: &mut ReadBuf<'_>,
            ) -> Poll<io::Result<()>> {
                Pin::new(self.get_mut().0.as_mut()).poll_read(cx, buf)
            }
        }

        impl<IO: AsyncRead + AsyncWrite + Unpin> AsyncWrite for $type<IO> {
            fn poll_write(
                self: Pin<&mut Self>,
                cx: &mut Context<'_>,
                data: &[u8],
            ) -> Poll<io::Result<usize>> {
                Pin::new(self.get_mut().0.as_mut()).poll_write(cx, data)
            }

            fn poll_flush(self: Pin<&mut Self>, cx: &mut Context<'_>) -> Poll<io::Result<()>> {
                Pin::new(self.get_mut().0.as_mut()).poll_flush(cx)
            }

            fn poll_shutdown(self: Pin<&mut Self>, cx: &mut Context<'_>) -> Poll<io::Result<()>> {
                Pin::new(self.get_mut().0.as_mut()).poll_shutdown(cx)
            }
        }
    };
}

delegate_io!(ServerStream);
delegate_io!(ClientStream);

/// Wall time for `rustls` from the caller's clock.
#[derive(Debug)]
struct RtTime(Arc<dyn WallClock>);

impl TimeProvider for RtTime {
    fn current_time(&self) -> Option<UnixTime> {
        let secs = self.0.unix_seconds()?;
        Some(UnixTime::since_unix_epoch(Duration::from_secs(secs)))
    }
}

/// Verifies a server chain to the CA certificate the chain presents whose
/// SubjectPublicKeyInfo has the pinned fingerprint, and the server name, an
/// IP address, among the leaf's `iPAddress` names (ARFC 0008 clause 3.8).
struct PinnedCa {
    pin: ApiCaFingerprint,
    provider: Arc<CryptoProvider>,
}

impl fmt::Debug for PinnedCa {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("PinnedCa")
            .field("pin", &self.pin)
            .finish_non_exhaustive()
    }
}

impl ServerCertVerifier for PinnedCa {
    fn verify_server_cert(
        &self,
        end_entity: &CertificateDer<'_>,
        intermediates: &[CertificateDer<'_>],
        server_name: &ServerName<'_>,
        ocsp_response: &[u8],
        now: UnixTime,
    ) -> Result<ServerCertVerified, rustls::Error> {
        let ca = intermediates
            .iter()
            .find(|c| ApiCaFingerprint::of_certificate(c) == Some(self.pin))
            .ok_or(rustls::Error::InvalidCertificate(
                CertificateError::UnknownIssuer,
            ))?;
        let mut roots = RootCertStore::empty();
        roots.add(ca.clone().into_owned())?;
        let verifier = WebPkiServerVerifier::builder_with_provider(
            Arc::new(roots),
            Arc::clone(&self.provider),
        )
        .build()
        .map_err(|e| rustls::Error::General(e.to_string()))?;
        verifier.verify_server_cert(end_entity, intermediates, server_name, ocsp_response, now)
    }

    fn verify_tls12_signature(
        &self,
        _message: &[u8],
        _cert: &CertificateDer<'_>,
        _dss: &DigitallySignedStruct,
    ) -> Result<HandshakeSignatureValid, rustls::Error> {
        // The configuration offers TLS 1.3 only.
        Err(rustls::Error::PeerIncompatible(
            rustls::PeerIncompatible::Tls12NotOffered,
        ))
    }

    fn verify_tls13_signature(
        &self,
        message: &[u8],
        cert: &CertificateDer<'_>,
        dss: &DigitallySignedStruct,
    ) -> Result<HandshakeSignatureValid, rustls::Error> {
        rustls::crypto::verify_tls13_signature(
            message,
            cert,
            dss,
            &self.provider.signature_verification_algorithms,
        )
    }

    fn supported_verify_schemes(&self) -> Vec<SignatureScheme> {
        // The API server signs its CertificateVerify with this scheme only
        // (ARFC 0008 clause 3.8, ADR 0048).
        vec![SignatureScheme::ECDSA_NISTP256_SHA256]
    }
}

#[cfg(test)]
mod tests;
