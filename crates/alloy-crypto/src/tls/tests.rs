//! Handshakes in memory with the blocking `rustls` connections over the
//! configurations of this module, so no socket and no async runtime is
//! needed.

use std::net::{IpAddr, Ipv4Addr};

use rustls::{ClientConnection, ServerConnection};

use super::*;
use crate::x509::{self, CertificateFields};

/// 2026-01-01T00:00:00Z.
const NOW: u64 = 1_767_225_600;

#[derive(Debug)]
struct Fixed(u64);

impl WallClock for Fixed {
    fn unix_seconds(&self) -> Option<u64> {
        Some(self.0)
    }
}

fn clock() -> Arc<dyn WallClock> {
    Arc::new(Fixed(NOW))
}

/// An API CA and a server certificate it issued for `ip`.
struct Pki {
    ca: Vec<u8>,
    cert: ServerCert,
}

fn pki(ca_scalar: u8, ip: IpAddr) -> Pki {
    let ca_key = SigningKey::from_scalar(&[ca_scalar; 32]).expect("ca key");
    let not_before = i64::try_from(NOW).expect("time") - 3_600;
    let ca = x509::self_signed_ca(
        &ca_key,
        &CertificateFields {
            serial: [1; 16],
            common_name: "Alloy API CA test",
            not_before,
            not_after: not_before + 86_400,
        },
    )
    .expect("ca");
    let leaf_key = SigningKey::from_scalar(&[0x22; 32]).expect("leaf key");
    let leaf = x509::issue_server(
        &ca_key,
        "Alloy API CA test",
        &leaf_key.public_key(),
        &CertificateFields {
            serial: [2; 16],
            common_name: "node-1",
            not_before,
            not_after: not_before + 86_400,
        },
        &[ip],
    )
    .expect("leaf");
    let cert = ServerCert::new(vec![leaf, ca.clone()], leaf_key).expect("server cert");
    Pki { ca, cert }
}

fn localhost() -> IpAddr {
    IpAddr::V4(Ipv4Addr::LOCALHOST)
}

/// Moves every byte one side has to send to the other until neither has
/// any, and returns the first error either side raised.
fn run(client: &mut ClientConnection, server: &mut ServerConnection) -> Result<(), rustls::Error> {
    for _ in 0..20 {
        let mut to_server = Vec::new();
        while client.wants_write() {
            client.write_tls(&mut to_server).expect("client write");
        }
        let mut to_client = Vec::new();
        while server.wants_write() {
            server.write_tls(&mut to_client).expect("server write");
        }
        if to_server.is_empty() && to_client.is_empty() {
            return Ok(());
        }
        if !to_server.is_empty() {
            server
                .read_tls(&mut to_server.as_slice())
                .expect("server read");
            server.process_new_packets()?;
        }
        if !to_client.is_empty() {
            client
                .read_tls(&mut to_client.as_slice())
                .expect("client read");
            client.process_new_packets()?;
        }
    }
    Ok(())
}

fn handshake(
    pki: &Pki,
    pin: ApiCaFingerprint,
    ip: IpAddr,
) -> Result<ClientConnection, rustls::Error> {
    let server = ServerTls::new(clock(), &pki.cert, ALPN_H2).expect("server");
    let client = ClientTls::pinned(clock(), pin, &[ALPN_H2]).expect("client");
    let mut client = ClientConnection::new(Arc::clone(&client.0), ServerName::IpAddress(ip.into()))
        .expect("client connection");
    let mut server = ServerConnection::new(Arc::clone(&server.0)).expect("server connection");
    run(&mut client, &mut server)?;
    Ok(client)
}

#[test]
fn a_client_completes_the_handshake_to_its_pinned_ca() {
    let pki = pki(0x11, localhost());
    let pin = ApiCaFingerprint::of_certificate(&pki.ca).expect("fingerprint");
    let client = handshake(&pki, pin, localhost()).expect("handshake");
    assert!(!client.is_handshaking());
    assert_eq!(client.alpn_protocol(), Some(ALPN_H2));
    assert_eq!(
        client.protocol_version(),
        Some(rustls::ProtocolVersion::TLSv1_3)
    );
}

#[test]
fn a_chain_that_fails_the_pin_or_the_address_is_refused() {
    let pki = pki(0x11, localhost());
    let pin = ApiCaFingerprint::of_certificate(&pki.ca).expect("fingerprint");
    let other_ca = ApiCaFingerprint::of_certificate(&pki_ca(0x33)).expect("fingerprint");
    for (pin, ip) in [
        (other_ca, localhost()),
        (pin, IpAddr::V4(Ipv4Addr::new(192, 0, 2, 9))),
    ] {
        let refused = handshake(&pki, pin, ip).map(|_| ());
        assert!(
            matches!(refused, Err(rustls::Error::InvalidCertificate(_))),
            "{refused:?}"
        );
    }
}

fn pki_ca(scalar: u8) -> Vec<u8> {
    pki(scalar, localhost()).ca
}

#[test]
fn a_server_cert_must_hold_its_leafs_key() {
    let good = pki(0x11, localhost());
    let other = SigningKey::from_scalar(&[0x44; 32]).expect("key");
    let leaf = {
        let ca_key = SigningKey::from_scalar(&[0x11; 32]).expect("ca key");
        x509::issue_server(
            &ca_key,
            "Alloy API CA test",
            &SigningKey::from_scalar(&[0x22; 32])
                .expect("leaf key")
                .public_key(),
            &CertificateFields {
                serial: [2; 16],
                common_name: "node-1",
                not_before: 0,
                not_after: 86_400,
            },
            &[localhost()],
        )
        .expect("leaf")
    };
    assert!(ServerCert::new(vec![leaf, good.ca], other).is_err());
    let key = SigningKey::from_scalar(&[0x44; 32]).expect("key");
    assert!(ServerCert::new(Vec::new(), key).is_err());
}

#[test]
fn the_hello_reader_finds_the_alpn_in_any_split() {
    let pki = pki(0x11, localhost());
    let pin = ApiCaFingerprint::of_certificate(&pki.ca).expect("fingerprint");
    let client = ClientTls::pinned(clock(), pin, &[ALPN_STREAM, ALPN_H2]).expect("client");
    let mut client = ClientConnection::new(
        Arc::clone(&client.0),
        ServerName::IpAddress(localhost().into()),
    )
    .expect("client connection");
    let mut hello = Vec::new();
    while client.wants_write() {
        client.write_tls(&mut hello).expect("write");
    }
    for split in [1, 5, hello.len() / 2, hello.len()] {
        let mut reader = HelloReader::new();
        let mut result = reader.feed(&hello[..split]);
        if split < hello.len() {
            assert!(matches!(result, Hello::Incomplete), "{result:?}");
            result = reader.feed(&hello[split..]);
        }
        let Hello::Complete(hello) = result else {
            panic!("a whole ClientHello: {result:?}");
        };
        assert_eq!(hello.alpn(), Some(vec![ALPN_STREAM, ALPN_H2]));
    }
}

#[test]
fn bytes_that_are_no_client_hello_are_refused() {
    let mut reader = HelloReader::new();
    let result = reader.feed(b"PRI * HTTP/2.0\r\n\r\nSM\r\n\r\n");
    assert!(matches!(result, Hello::Refused { .. }), "{result:?}");
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

#[test]
fn the_alert_is_no_application_protocol() {
    // RFC 8446 section 6: AlertDescription no_application_protocol(120),
    // AlertLevel fatal(2), in a record of ContentType alert(21).
    assert_eq!(
        NO_APPLICATION_PROTOCOL_ALERT,
        [0x15, 0x03, 0x03, 0x00, 0x02, 0x02, 0x78]
    );
    assert_eq!(
        u8::from(rustls::AlertDescription::NoApplicationProtocol),
        NO_APPLICATION_PROTOCOL
    );
}

#[test]
fn the_fingerprint_covers_the_whole_spki() {
    let key = SigningKey::from_scalar(&[0x11; 32]).expect("key");
    let spki = key.public_key().spki_der().expect("spki");
    assert_eq!(spki.len(), crate::ecdsa::SPKI_LEN);
    let ca = pki_ca(0x11);
    assert_eq!(
        ApiCaFingerprint::of_certificate(&ca),
        Some(ApiCaFingerprint::of_spki(&spki))
    );
    assert_eq!(ApiCaFingerprint::of_certificate(b"not a certificate"), None);
}
