//! TLS 1.3 of the API port (ARFC 0008 clauses 3.8 and 3.8.1).
//!
//! Every configuration here uses the `aws-lc-rs` provider (ADR 0053) with
//! only the key exchange groups `agent/eng/crypto.md` rule 4 lists, TLS 1.3
//! only, and wall time from the `alloy-rt` clock. Session tickets,
//! resumption, early data, and Encrypted Client Hello are off on both sides,
//! so the provider draws randomness only for the values of one handshake
//! (ADR 0042). No Alloy key is ever given to the provider: the server's key
//! arrives as a [`rustls::sign::SigningKey`] inside the caller's
//! [`ResolvesServerCert`].

use std::fmt;
use std::sync::Arc;

use alloy_rt::Clock;
use rustls::client::danger::{HandshakeSignatureValid, ServerCertVerified, ServerCertVerifier};
use rustls::client::{Resumption, WebPkiServerVerifier};
use rustls::crypto::{CryptoProvider, aws_lc_rs};
use rustls::pki_types::{CertificateDer, ServerName, UnixTime};
use rustls::server::{NoServerSessionStorage, ResolvesServerCert};
use rustls::time_provider::TimeProvider;
use rustls::{
    CertificateError, ClientConfig, DigitallySignedStruct, RootCertStore, ServerConfig,
    SignatureScheme,
};

/// The ALPN protocol id of the API (RFC 7301, RFC 9113).
pub const ALPN_H2: &[u8] = b"h2";

/// The ALPN protocol id of the `stream` underlay (ARFC 0004 section 3.6).
pub const ALPN_STREAM: &[u8] = b"alloy-al/1";

/// The bytes of a fatal TLS alert `no_application_protocol` (RFC 7301
/// section 3.2, RFC 8446 sections 5.1 and 6): one plaintext record of type
/// alert (21), legacy version 0x0303, length 2, level fatal (2), description
/// 120.
pub(crate) const NO_APPLICATION_PROTOCOL_ALERT: [u8; 7] = [21, 3, 3, 0, 2, 2, 120];

/// A protocol the API port serves, chosen by ALPN.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Protocol {
    /// The API, `h2`, on a node that holds `control`.
    Api,
    /// The `stream` underlay, `alloy-al/1`, on a node that holds `anchor`.
    Stream,
}

/// The protocol the API port answers a ClientHello with (ARFC 0008 clause
/// 3.8): `h2` when the node serves the API and the client offers it, else
/// `alloy-al/1` when the node serves `stream` and the client offers it, else
/// none, and then the server refuses the connection with
/// `no_application_protocol`. A ClientHello without ALPN gets none.
#[must_use]
pub fn select_protocol<'a>(
    offered: Option<impl Iterator<Item = &'a [u8]>>,
    api: bool,
    stream: bool,
) -> Option<Protocol> {
    let (mut h2, mut al) = (false, false);
    for id in offered.into_iter().flatten() {
        h2 |= id == ALPN_H2;
        al |= id == ALPN_STREAM;
    }
    if api && h2 {
        Some(Protocol::Api)
    } else if stream && al {
        Some(Protocol::Stream)
    } else {
        None
    }
}

/// Why a TLS configuration could not be built.
#[derive(Debug, thiserror::Error)]
#[error("the TLS configuration was refused: {0}")]
pub struct TlsError(#[from] rustls::Error);

/// The `aws-lc-rs` provider with the key exchange groups of
/// `agent/eng/crypto.md` rule 4: X25519MLKEM768, X25519, and secp256r1.
#[must_use]
pub fn provider() -> CryptoProvider {
    let mut provider = aws_lc_rs::default_provider();
    provider.kx_groups = vec![
        aws_lc_rs::kx_group::X25519MLKEM768,
        aws_lc_rs::kx_group::X25519,
        aws_lc_rs::kx_group::SECP256R1,
    ];
    provider
}

/// The server configuration of `protocol` on the API port: TLS 1.3 only,
/// that protocol's ALPN id only, the certificate `certs` resolves, no client
/// certificate, no session ticket, no resumption state, no early data.
///
/// # Errors
///
/// [`TlsError`] when `rustls` refuses the configuration.
pub fn server_config(
    clock: Arc<dyn Clock>,
    certs: Arc<dyn ResolvesServerCert>,
    protocol: Protocol,
) -> Result<Arc<ServerConfig>, TlsError> {
    let mut config =
        ServerConfig::builder_with_details(Arc::new(provider()), Arc::new(RtTime(clock)))
            .with_protocol_versions(&[&rustls::version::TLS13])?
            .with_no_client_auth()
            .with_cert_resolver(certs);
    config.alpn_protocols = vec![
        match protocol {
            Protocol::Api => ALPN_H2,
            Protocol::Stream => ALPN_STREAM,
        }
        .to_vec(),
    ];
    config.session_storage = Arc::new(NoServerSessionStorage {});
    config.send_tls13_tickets = 0;
    config.max_early_data_size = 0;
    config.send_half_rtt_data = false;
    Ok(Arc::new(config))
}

/// The client configuration of an API client (ARFC 0008 clause 3.8, ARFC 0002
/// section 3.7): TLS 1.3 only, ALPN `h2` only, no resumption, no early data,
/// no Encrypted Client Hello, and a server chain that must verify to a CA
/// certificate whose SubjectPublicKeyInfo has the fingerprint `api_ca_fp`.
///
/// # Errors
///
/// [`TlsError`] when `rustls` refuses the configuration.
pub fn client_config(
    clock: Arc<dyn Clock>,
    api_ca_fp: ApiCaFingerprint,
) -> Result<Arc<ClientConfig>, TlsError> {
    let provider = Arc::new(provider());
    let verifier = PinnedCa {
        pin: api_ca_fp,
        provider: Arc::clone(&provider),
    };
    let mut config =
        ClientConfig::builder_with_details(Arc::clone(&provider), Arc::new(RtTime(clock)))
            .with_protocol_versions(&[&rustls::version::TLS13])?
            .dangerous()
            .with_custom_certificate_verifier(Arc::new(verifier))
            .with_no_client_auth();
    config.alpn_protocols = vec![ALPN_H2.to_vec()];
    config.resumption = Resumption::disabled();
    config.enable_early_data = false;
    Ok(Arc::new(config))
}

/// `api_ca_fp` (ARFC 0002 section 3.7): the first 16 bytes of the BLAKE3 hash
/// of the DER encoding of the API CA certificate's SubjectPublicKeyInfo.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub struct ApiCaFingerprint(pub [u8; 16]);

impl ApiCaFingerprint {
    /// The fingerprint of a certificate's SubjectPublicKeyInfo, or `None`
    /// when the certificate does not parse.
    #[must_use]
    pub fn of_certificate(cert: &CertificateDer<'_>) -> Option<ApiCaFingerprint> {
        let anchor = webpki::anchor_from_trusted_cert(cert).ok()?;
        let spki = webpki::spki_for_anchor(&anchor);
        Some(ApiCaFingerprint::of_spki(spki.as_ref()))
    }

    /// The fingerprint of a DER-encoded SubjectPublicKeyInfo.
    #[must_use]
    pub fn of_spki(spki_der: &[u8]) -> ApiCaFingerprint {
        let mut fp = [0; 16];
        fp.copy_from_slice(&alloy_crypto::hash(spki_der)[..16]);
        ApiCaFingerprint(fp)
    }
}

/// Wall time for `rustls` from the `alloy-rt` clock.
#[derive(Debug)]
struct RtTime(Arc<dyn Clock>);

impl TimeProvider for RtTime {
    fn current_time(&self) -> Option<UnixTime> {
        let nanos = self.0.wall().0;
        let secs = u64::try_from(nanos / 1_000_000_000).ok()?;
        Some(UnixTime::since_unix_epoch(std::time::Duration::from_secs(
            secs,
        )))
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
mod tests {
    use super::*;

    fn offer<'a>(ids: &'a [&'static [u8]]) -> Option<impl Iterator<Item = &'static [u8]> + 'a> {
        Some(ids.iter().copied())
    }

    #[test]
    fn alpn_selection_follows_clause_3_8() {
        let both: [&[u8]; 2] = [ALPN_STREAM, ALPN_H2];
        assert_eq!(
            select_protocol(offer(&both), true, true),
            Some(Protocol::Api)
        );
        assert_eq!(
            select_protocol(offer(&both), false, true),
            Some(Protocol::Stream)
        );
        assert_eq!(select_protocol(offer(&[ALPN_H2]), false, true), None);
        assert_eq!(select_protocol(offer(&[ALPN_STREAM]), true, false), None);
        assert_eq!(select_protocol(offer(&[b"http/1.1"]), true, true), None);
        assert_eq!(select_protocol(offer(&[]), true, true), None);
        assert_eq!(
            select_protocol(None::<std::vec::IntoIter<&[u8]>>, true, true),
            None
        );
    }

    #[test]
    fn the_provider_offers_only_the_listed_groups() {
        let names: Vec<_> = provider().kx_groups.iter().map(|g| g.name()).collect();
        assert_eq!(
            names,
            [
                rustls::NamedGroup::X25519MLKEM768,
                rustls::NamedGroup::X25519,
                rustls::NamedGroup::secp256r1
            ]
        );
    }
}
