//! X.509 v3 certificates for the API CA and the API server (ARFC 0002
//! section 2), built with `rcgen` and signed through
//! [`crate::ecdsa::SigningKey`] (ADR 0053).
//!
//! Two profiles exist, and a certificate carries exactly the extensions of
//! its profile:
//!
//! | Extension | [`self_signed_ca`] | [`issue_server`] |
//! |---|---|---|
//! | subject key identifier | RFC 7093 method 1 | RFC 7093 method 1 |
//! | authority key identifier | none | the issuer's subject key identifier |
//! | basic constraints, critical | `cA` true, path length 0 | `cA` false |
//! | key usage, critical | `keyCertSign`, `cRLSign` | `digitalSignature` |
//! | extended key usage | none | `id-kp-serverAuth` |
//! | subject alternative name | none | one `iPAddress` per address given |
//!
//! Every certificate is signed `ecdsa-with-SHA256` with an RFC 6979 nonce,
//! names its subject and issuer by a common name alone, and carries the
//! serial number and validity the caller gives.

use std::net::IpAddr;

use rcgen::{
    BasicConstraints, CertificateParams, DistinguishedName, DnType, ExtendedKeyUsagePurpose, IsCa,
    Issuer, KeyIdMethod, KeyUsagePurpose, PKCS_ECDSA_P256_SHA256, PublicKeyData, SanType,
    SerialNumber, SignatureAlgorithm,
};
use time::OffsetDateTime;

use crate::ecdsa::{POINT_LEN, PublicKey, SigningKey};

/// The length of a serial number this module writes.
pub const SERIAL_LEN: usize = 16;

/// Why a certificate could not be built.
#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
pub enum X509Error {
    /// A validity time is outside what a certificate can carry, or
    /// `not_before` is not before `not_after`.
    #[error("the validity period cannot be encoded")]
    Validity,
    /// The serial number's high bit is set, so it would encode as a
    /// negative or longer integer.
    #[error("the serial number's high bit is set")]
    Serial,
    /// `rcgen` refused the parameters, for example a name it cannot encode.
    #[error("the certificate could not be encoded: {0}")]
    Encode(String),
}

/// The fields a caller chooses for a certificate.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct CertificateFields<'a> {
    /// The serial number, big endian, with the high bit of the first byte
    /// clear.
    pub serial: [u8; SERIAL_LEN],
    /// The subject's common name.
    pub common_name: &'a str,
    /// The start of validity, in seconds since 1970-01-01T00:00:00Z.
    pub not_before: i64,
    /// The end of validity, in seconds since 1970-01-01T00:00:00Z.
    pub not_after: i64,
}

/// Returns the DER of the self-signed CA certificate of `key` with `fields`,
/// in the CA profile of the module table.
///
/// # Errors
///
/// [`X509Error::Validity`], [`X509Error::Serial`], and
/// [`X509Error::Encode`].
pub fn self_signed_ca(
    key: &SigningKey,
    fields: &CertificateFields<'_>,
) -> Result<Vec<u8>, X509Error> {
    let signer = Signer::new(key);
    let params = ca_params(fields.common_name, &signer.key_id(), Some(fields))?;
    params
        .self_signed(&signer)
        .map(|certificate| certificate.der().to_vec())
        .map_err(encode_error)
}

/// Returns the DER of a server certificate for `subject`, issued by the CA
/// whose key is `ca_key` and whose common name is `ca_common_name`, in the
/// server profile of the module table, with one `iPAddress` name per entry of
/// `ip_addresses`, in that order.
///
/// # Errors
///
/// [`X509Error::Validity`], [`X509Error::Serial`], and
/// [`X509Error::Encode`], also for an empty `ip_addresses`.
pub fn issue_server(
    ca_key: &SigningKey,
    ca_common_name: &str,
    subject: &PublicKey,
    fields: &CertificateFields<'_>,
    ip_addresses: &[IpAddr],
) -> Result<Vec<u8>, X509Error> {
    if ip_addresses.is_empty() {
        return Err(X509Error::Encode("no subject alternative name".to_owned()));
    }
    let ca_signer = Signer::new(ca_key);
    let ca_params = ca_params(ca_common_name, &ca_signer.key_id(), None)?;
    let issuer = Issuer::from_params(&ca_params, &ca_signer);

    let subject_key = SubjectKey::new(subject);
    let mut params = CertificateParams::default();
    set_common_fields(&mut params, fields, &subject.key_identifier())?;
    params.is_ca = IsCa::ExplicitNoCa;
    params.key_usages = vec![KeyUsagePurpose::DigitalSignature];
    params.extended_key_usages = vec![ExtendedKeyUsagePurpose::ServerAuth];
    params.subject_alt_names = ip_addresses
        .iter()
        .map(|ip| SanType::IpAddress(*ip))
        .collect();
    params.use_authority_key_identifier_extension = true;
    params
        .signed_by(&subject_key, &issuer)
        .map(|certificate| certificate.der().to_vec())
        .map_err(encode_error)
}

/// The parameters of a CA certificate. Without `fields` they serve only to
/// name an issuer: its common name and key identifier.
fn ca_params(
    common_name: &str,
    key_id: &[u8],
    fields: Option<&CertificateFields<'_>>,
) -> Result<CertificateParams, X509Error> {
    let mut params = CertificateParams::default();
    params.distinguished_name = common_name_only(common_name);
    params.key_identifier_method = KeyIdMethod::PreSpecified(key_id.to_vec());
    params.is_ca = IsCa::Ca(BasicConstraints::Constrained(0));
    params.key_usages = vec![KeyUsagePurpose::KeyCertSign, KeyUsagePurpose::CrlSign];
    if let Some(fields) = fields {
        set_common_fields(&mut params, fields, key_id)?;
    }
    Ok(params)
}

fn set_common_fields(
    params: &mut CertificateParams,
    fields: &CertificateFields<'_>,
    key_id: &[u8],
) -> Result<(), X509Error> {
    if fields.serial[0] & 0x80 != 0 {
        return Err(X509Error::Serial);
    }
    if fields.not_before >= fields.not_after {
        return Err(X509Error::Validity);
    }
    params.serial_number = Some(SerialNumber::from_slice(&fields.serial));
    params.not_before = time(fields.not_before)?;
    params.not_after = time(fields.not_after)?;
    params.distinguished_name = common_name_only(fields.common_name);
    params.key_identifier_method = KeyIdMethod::PreSpecified(key_id.to_vec());
    Ok(())
}

fn common_name_only(common_name: &str) -> DistinguishedName {
    let mut name = DistinguishedName::new();
    name.push(DnType::CommonName, common_name);
    name
}

fn time(seconds: i64) -> Result<OffsetDateTime, X509Error> {
    OffsetDateTime::from_unix_timestamp(seconds).map_err(|_| X509Error::Validity)
}

fn encode_error(error: rcgen::Error) -> X509Error {
    X509Error::Encode(error.to_string())
}

/// A P-256 key as `rcgen` signs with it. `rcgen` only borrows the key, never
/// copies it.
struct Signer<'a> {
    key: &'a SigningKey,
    point: [u8; POINT_LEN],
}

impl<'a> Signer<'a> {
    fn new(key: &'a SigningKey) -> Self {
        Self {
            key,
            point: key.public_key().to_uncompressed(),
        }
    }

    fn key_id(&self) -> [u8; crate::ecdsa::KEY_ID_LEN] {
        self.key.public_key().key_identifier()
    }
}

impl PublicKeyData for Signer<'_> {
    fn der_bytes(&self) -> &[u8] {
        &self.point
    }

    fn algorithm(&self) -> &'static SignatureAlgorithm {
        &PKCS_ECDSA_P256_SHA256
    }
}

impl rcgen::SigningKey for Signer<'_> {
    fn sign(&self, msg: &[u8]) -> Result<Vec<u8>, rcgen::Error> {
        Ok(self.key.sign_der(msg))
    }
}

/// The public key of a certificate's subject.
struct SubjectKey {
    point: [u8; POINT_LEN],
}

impl SubjectKey {
    fn new(key: &PublicKey) -> Self {
        Self {
            point: key.to_uncompressed(),
        }
    }
}

impl PublicKeyData for SubjectKey {
    fn der_bytes(&self) -> &[u8] {
        &self.point
    }

    fn algorithm(&self) -> &'static SignatureAlgorithm {
        &PKCS_ECDSA_P256_SHA256
    }
}
