//! The API CA and the API server certificate (ARFC 0002 section 2, ARFC 0008
//! clause 3.8).

use std::net::{IpAddr, Ipv4Addr, Ipv6Addr};

use alloy_crypto::Zeroizing;
use alloy_crypto::ecdsa::{self, SigningKey};
use alloy_crypto::x509::{self, CertificateFields, SERIAL_LEN};
use alloy_rt::{Rng, WallTime};

use crate::IdError;
use crate::ids::ClusterId;
use crate::keys::generate_p256;
use crate::record::seconds;

/// Both certificates start 1 hour before they are issued.
const BACKDATE_SECS: i64 = 3_600;

/// An API server certificate is valid for 30 days after issue.
pub const SERVER_VALIDITY_SECS: i64 = 30 * 86_400;

/// A control node replaces its API server certificate when 10 days of its
/// validity remain.
pub const SERVER_RENEW_BEFORE_SECS: i64 = 10 * 86_400;

/// The API CA certificate is valid for 10 years after issue.
pub const CA_VALIDITY_YEARS: i32 = 10;

/// The API CA: its key and its self-signed certificate.
#[derive(Debug)]
pub struct ApiCa {
    key: SigningKey,
    common_name: String,
    certificate: Vec<u8>,
}

/// An API server key and the certificate the API CA issued for it.
#[derive(Debug)]
pub struct ApiServerCertificate {
    /// The key, which signs the TLS 1.3 `CertificateVerify`.
    pub key: SigningKey,
    /// The certificate, DER.
    pub certificate: Vec<u8>,
    /// The end of its validity, seconds since 1970.
    pub not_after: i64,
}

impl ApiServerCertificate {
    /// When the control node replaces it: 10 days before `not_after`.
    #[must_use]
    pub fn renew_at(&self) -> i64 {
        self.not_after.saturating_sub(SERVER_RENEW_BEFORE_SECS)
    }
}

impl ApiCa {
    /// Generates the API CA of `cluster_id` at `now`: a new P-256 key from
    /// `rng`, and a self-signed certificate with subject `CN=Alloy API CA
    /// <cluster_id>`, a 16-byte serial number from `rng`, and validity from 1
    /// hour before `now` to 10 years after.
    ///
    /// # Errors
    ///
    /// [`IdError::Rng`], [`IdError::Generator`], and [`IdError::X509`].
    pub fn generate(rng: &dyn Rng, cluster_id: &ClusterId, now: WallTime) -> Result<Self, IdError> {
        let key = generate_p256(rng)?;
        let common_name = format!("Alloy API CA {cluster_id}");
        let issued = seconds(now);
        let fields = CertificateFields {
            serial: serial(rng)?,
            common_name: &common_name,
            not_before: issued.saturating_sub(BACKDATE_SECS),
            not_after: add_years(issued, CA_VALIDITY_YEARS)?,
        };
        let certificate = x509::self_signed_ca(&key, &fields)?;
        Ok(Self {
            key,
            common_name,
            certificate,
        })
    }

    /// The API CA of `cluster_id` from its stored key, PKCS#8 DER, and its
    /// stored certificate, DER (section 3.14).
    ///
    /// # Errors
    ///
    /// [`IdError::Ecdsa`] when the key does not decode.
    pub fn from_stored(
        cluster_id: &ClusterId,
        key_pkcs8: &[u8],
        certificate: Vec<u8>,
    ) -> Result<Self, IdError> {
        Ok(Self {
            key: SigningKey::from_pkcs8_der(key_pkcs8)?,
            common_name: format!("Alloy API CA {cluster_id}"),
            certificate,
        })
    }

    /// The key as PKCS#8 DER, for `NodeSecrets.api_ca_key`.
    ///
    /// # Errors
    ///
    /// [`IdError::Ecdsa`] if encoding fails.
    pub fn key_pkcs8(&self) -> Result<Zeroizing<Vec<u8>>, IdError> {
        Ok(self.key.to_pkcs8_der()?)
    }

    /// The certificate, DER.
    #[must_use]
    pub fn certificate(&self) -> &[u8] {
        &self.certificate
    }

    /// The SHA-256 key identifier of the CA key, which every certificate it
    /// issues names as its authority key identifier.
    #[must_use]
    pub fn key_identifier(&self) -> [u8; ecdsa::KEY_ID_LEN] {
        self.key.public_key().key_identifier()
    }

    /// Issues a server certificate for the node `node_name` at `now`: a new
    /// P-256 key from `rng`, subject `CN=<node_name>`, a 16-byte serial
    /// number from `rng`, validity from 1 hour before `now` to 30 days
    /// after, and one `iPAddress` name per address of `advertise`, then
    /// `127.0.0.1` and `::1`.
    ///
    /// # Errors
    ///
    /// [`IdError::Rng`], [`IdError::Generator`], and [`IdError::X509`].
    pub fn issue_server(
        &self,
        rng: &dyn Rng,
        node_name: &str,
        advertise: &[IpAddr],
        now: WallTime,
    ) -> Result<ApiServerCertificate, IdError> {
        let key = generate_p256(rng)?;
        let issued = seconds(now);
        let not_after = issued.saturating_add(SERVER_VALIDITY_SECS);
        let fields = CertificateFields {
            serial: serial(rng)?,
            common_name: node_name,
            not_before: issued.saturating_sub(BACKDATE_SECS),
            not_after,
        };
        let mut names = advertise.to_vec();
        names.push(IpAddr::V4(Ipv4Addr::LOCALHOST));
        names.push(IpAddr::V6(Ipv6Addr::LOCALHOST));
        let certificate = x509::issue_server(
            &self.key,
            &self.common_name,
            &key.public_key(),
            &fields,
            &names,
        )?;
        Ok(ApiServerCertificate {
            key,
            certificate,
            not_after,
        })
    }
}

/// 16 bytes from `rng` with the high bit of the first byte cleared.
fn serial(rng: &dyn Rng) -> Result<[u8; SERIAL_LEN], IdError> {
    let mut serial = [0u8; SERIAL_LEN];
    rng.fill(&mut serial)?;
    serial[0] &= 0x7f;
    Ok(serial)
}

/// `seconds` moved `years` calendar years later, at the same time of day. A
/// 29 February with no counterpart becomes 28 February.
fn add_years(seconds: i64, years: i32) -> Result<i64, IdError> {
    let start = time_of(seconds)?;
    let year = start.year().checked_add(years).ok_or(IdError::Time)?;
    let date = start
        .date()
        .replace_year(year)
        .or_else(|_| {
            start
                .date()
                .replace_day(28)
                .and_then(|date| date.replace_year(year))
        })
        .map_err(|_| IdError::Time)?;
    Ok(start.replace_date(date).unix_timestamp())
}

fn time_of(seconds: i64) -> Result<time::OffsetDateTime, IdError> {
    time::OffsetDateTime::from_unix_timestamp(seconds).map_err(|_| IdError::Time)
}
