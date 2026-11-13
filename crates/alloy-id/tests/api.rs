//! The API CA and API server certificates of ARFC 0002 section 2, generated
//! from the simulator's randomness trait.

mod common;

use std::net::{IpAddr, Ipv4Addr};

use alloy_id::api::{SERVER_RENEW_BEFORE_SECS, SERVER_VALIDITY_SECS};
use alloy_id::{ApiCa, ClusterId};
use alloy_rt::WallTime;
use common::{err, on_host};

/// 2024-02-29T12:00:00Z.
const LEAP_DAY: i64 = 1_709_208_000;

fn contains(haystack: &[u8], needle: &[u8]) -> bool {
    haystack.windows(needle.len()).any(|w| w == needle)
}

#[test]
fn ca_and_server_certificates() {
    on_host(4, |runtime| async move {
        let rng = runtime.rng().as_ref();
        let cluster_id = ClusterId::from_bytes([0xab; 16]);
        let now = WallTime(LEAP_DAY * 1_000_000_000);
        let ca = ApiCa::generate(rng, &cluster_id, now).map_err(err("ca"))?;
        let subject = format!("Alloy API CA {}", "ab".repeat(16));
        if !contains(ca.certificate(), subject.as_bytes()) {
            return Err("CA subject".to_owned());
        }
        // 1 hour before issue, and 10 calendar years after: 2034-02-28,
        // since 2034 has no 29 February.
        if !contains(ca.certificate(), b"240229110000Z")
            || !contains(ca.certificate(), b"340228120000Z")
        {
            return Err("CA validity".to_owned());
        }

        // The stored key and certificate give the same CA.
        let pkcs8 = ca.key_pkcs8().map_err(err("pkcs8"))?;
        let stored = ApiCa::from_stored(&cluster_id, &pkcs8, ca.certificate().to_vec())
            .map_err(err("stored"))?;
        if stored.key_identifier() != ca.key_identifier() {
            return Err("stored CA differs".to_owned());
        }

        let advertise = [IpAddr::V4(Ipv4Addr::new(203, 0, 113, 9))];
        let server = stored
            .issue_server(rng, "node-1", &advertise, now)
            .map_err(err("server"))?;
        if server.not_after != LEAP_DAY + SERVER_VALIDITY_SECS
            || server.renew_at() != server.not_after - SERVER_RENEW_BEFORE_SECS
        {
            return Err("server validity".to_owned());
        }
        let cert = &server.certificate;
        // The issuer is the CA's subject, the authority key identifier its
        // key identifier, and the names the advertise address then loopback.
        if !contains(cert, subject.as_bytes())
            || !contains(cert, b"node-1")
            || !contains(cert, &ca.key_identifier())
            || !contains(
                cert,
                &[0x87, 4, 203, 0, 113, 9, 0x87, 4, 127, 0, 0, 1, 0x87, 16],
            )
            || !contains(cert, b"240330120000Z")
        {
            return Err("server certificate fields".to_owned());
        }
        // Each certificate draws a new key and serial number.
        let other = ca
            .issue_server(rng, "node-1", &advertise, now)
            .map_err(err("again"))?;
        if other.certificate == server.certificate {
            return Err("two certificates are the same".to_owned());
        }
        Ok(())
    });
}
