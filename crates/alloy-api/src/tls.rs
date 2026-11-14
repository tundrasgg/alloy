//! TLS 1.3 of the API port (ARFC 0008 clauses 3.8 and 3.8.1), through
//! `alloy-crypto`, the only crate that depends on `rustls`
//! (`agent/eng/crypto.md` rule 1).
//!
//! `alloy-crypto` builds every configuration on the `aws-lc-rs` provider
//! (ADR 0053), TLS 1.3 only, with session tickets, resumption, early data,
//! and Encrypted Client Hello off on both sides (ADR 0042). This module adds
//! the API port's choices: the ALPN protocol of each configuration, the
//! choice between the API and `stream` (clause 3.8), and wall time from the
//! `alloy-rt` clock.

use std::sync::Arc;

use alloy_crypto::tls::WallClock;
pub use alloy_crypto::tls::{
    ALPN_H2, ALPN_STREAM, ApiCaFingerprint, ClientTls, ServerCert, ServerTls, TlsError,
};
use alloy_rt::Clock;

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
pub fn select_protocol(offered: Option<&[&[u8]]>, api: bool, stream: bool) -> Option<Protocol> {
    let offered = offered.unwrap_or_default();
    let h2 = offered.contains(&ALPN_H2);
    let al = offered.contains(&ALPN_STREAM);
    if api && h2 {
        Some(Protocol::Api)
    } else if stream && al {
        Some(Protocol::Stream)
    } else {
        None
    }
}

/// The server configuration of `protocol` on the API port: TLS 1.3 only,
/// that protocol's ALPN id only, `cert` presented, no client certificate, no
/// session ticket, no resumption state, no early data.
///
/// # Errors
///
/// [`TlsError`] when `rustls` refuses the configuration.
pub fn server_config(
    clock: Arc<dyn Clock>,
    cert: &ServerCert,
    protocol: Protocol,
) -> Result<ServerTls, TlsError> {
    let alpn = match protocol {
        Protocol::Api => ALPN_H2,
        Protocol::Stream => ALPN_STREAM,
    };
    ServerTls::new(Arc::new(RtWall(clock)), cert, alpn)
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
) -> Result<ClientTls, TlsError> {
    ClientTls::pinned(Arc::new(RtWall(clock)), api_ca_fp, &[ALPN_H2])
}

/// Wall time for certificate validity from the `alloy-rt` clock.
#[derive(Debug)]
pub(crate) struct RtWall(pub(crate) Arc<dyn Clock>);

impl WallClock for RtWall {
    fn unix_seconds(&self) -> Option<u64> {
        u64::try_from(self.0.wall().0 / 1_000_000_000).ok()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn alpn_selection_follows_clause_3_8() {
        let both: [&[u8]; 2] = [ALPN_STREAM, ALPN_H2];
        assert_eq!(
            select_protocol(Some(&both), true, true),
            Some(Protocol::Api)
        );
        assert_eq!(
            select_protocol(Some(&both), false, true),
            Some(Protocol::Stream)
        );
        assert_eq!(select_protocol(Some(&[ALPN_H2]), false, true), None);
        assert_eq!(select_protocol(Some(&[ALPN_STREAM]), true, false), None);
        assert_eq!(select_protocol(Some(&[b"http/1.1"]), true, true), None);
        assert_eq!(select_protocol(Some(&[]), true, true), None);
        assert_eq!(select_protocol(None, true, true), None);
    }
}
