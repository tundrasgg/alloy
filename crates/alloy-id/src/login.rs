//! Login codes (ARFC 0002 section 3.7).

use std::fmt;
use std::net::{IpAddr, Ipv4Addr, Ipv6Addr, SocketAddr};

use alloy_crypto::{Zeroize, Zeroizing};
use alloy_rt::Rng;

use crate::ids::ClusterId;
use crate::{IdError, base32};

/// The prefix of every login code string.
pub const CODE_PREFIX: &str = "alyc_";

/// The longest login code string, in characters: 4 IPv6 endpoints.
pub const MAX_CODE_LEN: usize = 226;

/// The most endpoints a code lists.
pub const MAX_ENDPOINTS: usize = 4;

const VERSION: u8 = 1;
const CHECKSUM_LEN: usize = 4;
/// version, cluster_id, api_ca_fp, code_id, secret, endpoint count.
const HEADER_LEN: usize = 1 + 16 + 16 + 8 + 16 + 1;

/// A login code's public lookup id.
#[derive(Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash, Debug)]
pub struct CodeId(pub [u8; 8]);

/// A login code's 16-byte secret. The control plane stores only its BLAKE3
/// hash. It erases itself when dropped and shows no bytes through `Debug`.
pub struct CodeSecret(Zeroizing<[u8; 16]>);

impl CodeSecret {
    /// Draws a new secret, as the client that asks for a code does.
    ///
    /// # Errors
    ///
    /// [`IdError::Rng`] when the generator fails.
    pub fn generate(rng: &dyn Rng) -> Result<Self, IdError> {
        let mut secret = Zeroizing::new([0u8; 16]);
        rng.fill(&mut secret[..])?;
        Ok(Self(secret))
    }

    /// The secret with these bytes.
    #[must_use]
    pub fn from_bytes(bytes: [u8; 16]) -> Self {
        Self(Zeroizing::new(bytes))
    }

    /// The 16 bytes of the secret.
    #[must_use]
    pub fn as_bytes(&self) -> &[u8; 16] {
        &self.0
    }

    /// `BLAKE3(secret)`, all of it the control plane is sent and stores.
    #[must_use]
    pub fn hash(&self) -> [u8; 32] {
        alloy_crypto::hash(&self.0[..])
    }
}

impl fmt::Debug for CodeSecret {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("CodeSecret").finish_non_exhaustive()
    }
}

/// A decoded login code.
#[derive(Debug)]
pub struct LoginCode {
    /// The cluster's id.
    pub cluster_id: ClusterId,
    /// The first 16 bytes of `BLAKE3` over the API CA certificate's
    /// SubjectPublicKeyInfo, DER.
    pub api_ca_fp: [u8; 16],
    /// The public lookup id.
    pub code_id: CodeId,
    /// The secret.
    pub secret: CodeSecret,
    /// The API endpoints, 1 to 4: the first advertise address of each of up
    /// to 4 control nodes, in ascending order of node name, with its API
    /// port.
    pub endpoints: Vec<SocketAddr>,
}

/// Why a string is not a valid login code. Each is a usage error, found
/// before any machine is contacted (section 3.7).
#[derive(Debug, Clone, Copy, PartialEq, Eq, thiserror::Error)]
pub enum CodeError {
    /// It does not start with `alyc_`.
    #[error("a login code starts with `alyc_`")]
    Prefix,
    /// It is longer than 226 characters.
    #[error("a login code is at most 226 characters")]
    TooLong,
    /// The text after `alyc_` is not unpadded lower case base32.
    #[error("the login code is not valid base32")]
    Encoding,
    /// The checksum does not match.
    #[error("the login code's checksum does not match")]
    Checksum,
    /// The version is not 1.
    #[error("the login code's version is not 1")]
    Version,
    /// A count or family is out of range, or the fields do not use every
    /// byte exactly.
    #[error("the login code's fields are malformed")]
    Malformed,
    /// No endpoint, or more than 4, to encode (encoding only).
    #[error("a login code lists 1 to 4 endpoints")]
    Endpoints,
}

impl LoginCode {
    /// Encodes the code as its `alyc_` string, with the endpoints in the
    /// order given.
    ///
    /// # Errors
    ///
    /// [`CodeError::Endpoints`] for no endpoint or more than 4.
    pub fn encode(&self) -> Result<String, CodeError> {
        if !(1..=MAX_ENDPOINTS).contains(&self.endpoints.len()) {
            return Err(CodeError::Endpoints);
        }
        let mut bytes = Zeroizing::new(Vec::with_capacity(HEADER_LEN + 4 * 19 + CHECKSUM_LEN));
        bytes.push(VERSION);
        bytes.extend_from_slice(self.cluster_id.as_bytes());
        bytes.extend_from_slice(&self.api_ca_fp);
        bytes.extend_from_slice(&self.code_id.0);
        bytes.extend_from_slice(self.secret.as_bytes());
        // 1 to 4 by the check above.
        bytes.push(self.endpoints.len() as u8);
        for endpoint in &self.endpoints {
            match endpoint.ip() {
                IpAddr::V4(ip) => {
                    bytes.push(4);
                    bytes.extend_from_slice(&ip.octets());
                }
                IpAddr::V6(ip) => {
                    bytes.push(6);
                    bytes.extend_from_slice(&ip.octets());
                }
            }
            bytes.extend_from_slice(&endpoint.port().to_be_bytes());
        }
        let checksum = alloy_crypto::hash(&bytes);
        bytes.extend_from_slice(&checksum[..CHECKSUM_LEN]);
        let mut text = base32::encode(&bytes);
        let code = format!("{CODE_PREFIX}{text}");
        text.zeroize();
        Ok(code)
    }

    /// Decodes a login code string, checking every rule of section 3.7, as
    /// for a join token (section 3.2), before anything is contacted.
    ///
    /// # Errors
    ///
    /// The [`CodeError`] of the first rule broken, in the order prefix,
    /// length, encoding, checksum, version, then fields.
    pub fn decode(text: &str) -> Result<Self, CodeError> {
        let rest = text.strip_prefix(CODE_PREFIX).ok_or(CodeError::Prefix)?;
        if text.len() > MAX_CODE_LEN {
            return Err(CodeError::TooLong);
        }
        let bytes = Zeroizing::new(base32::decode(rest).ok_or(CodeError::Encoding)?);
        let body_len = bytes
            .len()
            .checked_sub(CHECKSUM_LEN)
            .ok_or(CodeError::Malformed)?;
        let (body, checksum) = bytes.split_at(body_len);
        if alloy_crypto::hash(body)[..CHECKSUM_LEN] != *checksum {
            return Err(CodeError::Checksum);
        }
        let mut reader = Reader(body);
        if reader.take::<1>()?[0] != VERSION {
            return Err(CodeError::Version);
        }
        let cluster_id = ClusterId::from_bytes(reader.take()?);
        let api_ca_fp = reader.take()?;
        let code_id = CodeId(reader.take()?);
        let secret = CodeSecret::from_bytes(reader.take()?);
        let count = usize::from(reader.take::<1>()?[0]);
        if !(1..=MAX_ENDPOINTS).contains(&count) {
            return Err(CodeError::Malformed);
        }
        let mut endpoints = Vec::with_capacity(count);
        for _ in 0..count {
            let ip = match reader.take::<1>()?[0] {
                4 => IpAddr::V4(Ipv4Addr::from(reader.take::<4>()?)),
                6 => IpAddr::V6(Ipv6Addr::from(reader.take::<16>()?)),
                _ => return Err(CodeError::Malformed),
            };
            let port = u16::from_be_bytes(reader.take()?);
            endpoints.push(SocketAddr::new(ip, port));
        }
        if !reader.0.is_empty() {
            return Err(CodeError::Malformed);
        }
        Ok(Self {
            cluster_id,
            api_ca_fp,
            code_id,
            secret,
            endpoints,
        })
    }
}

/// Reads fixed-size fields from the front of a byte slice.
struct Reader<'a>(&'a [u8]);

impl Reader<'_> {
    fn take<const N: usize>(&mut self) -> Result<[u8; N], CodeError> {
        let (head, rest) = self
            .0
            .split_first_chunk::<N>()
            .ok_or(CodeError::Malformed)?;
        self.0 = rest;
        Ok(*head)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn code(endpoints: Vec<SocketAddr>) -> LoginCode {
        LoginCode {
            cluster_id: ClusterId::from_bytes([1; 16]),
            api_ca_fp: [2; 16],
            code_id: CodeId([3; 8]),
            secret: CodeSecret::from_bytes([4; 16]),
            endpoints,
        }
    }

    fn v6(n: u16) -> SocketAddr {
        SocketAddr::new(IpAddr::V6(Ipv6Addr::new(0xfd00, 0, 0, 0, 0, 0, 0, n)), 443)
    }

    #[test]
    fn a_code_round_trips() {
        let endpoints = vec![
            SocketAddr::new(IpAddr::V4(Ipv4Addr::new(192, 168, 1, 20)), 443),
            v6(2),
        ];
        let text = code(endpoints.clone()).encode().unwrap();
        assert!(text.starts_with(CODE_PREFIX));
        let back = LoginCode::decode(&text).unwrap();
        assert_eq!(back.cluster_id, ClusterId::from_bytes([1; 16]));
        assert_eq!(back.api_ca_fp, [2; 16]);
        assert_eq!(back.code_id, CodeId([3; 8]));
        assert_eq!(back.secret.as_bytes(), &[4; 16]);
        assert_eq!(back.endpoints, endpoints);
    }

    #[test]
    fn four_ipv6_endpoints_fill_the_longest_code() {
        let text = code((1..=4).map(v6).collect()).encode().unwrap();
        assert_eq!(text.len(), MAX_CODE_LEN);
        assert!(LoginCode::decode(&text).is_ok());
        assert_eq!(code(Vec::new()).encode().unwrap_err(), CodeError::Endpoints);
        assert_eq!(
            code((1..=5).map(v6).collect()).encode().unwrap_err(),
            CodeError::Endpoints
        );
    }

    #[test]
    fn every_rule_refuses() {
        let text = code(vec![v6(1)]).encode().unwrap();
        assert_eq!(
            LoginCode::decode(&text.replace("alyc_", "alyt_")).unwrap_err(),
            CodeError::Prefix
        );
        let long = format!("{CODE_PREFIX}{}", "a".repeat(MAX_CODE_LEN));
        assert_eq!(LoginCode::decode(&long).unwrap_err(), CodeError::TooLong);
        assert_eq!(
            LoginCode::decode("alyc_ABC").unwrap_err(),
            CodeError::Encoding
        );
        let mut flipped = text.clone().into_bytes();
        let at = flipped.len() - 10;
        flipped[at] = if flipped[at] == b'a' { b'b' } else { b'a' };
        let flipped = String::from_utf8(flipped).unwrap();
        assert_eq!(
            LoginCode::decode(&flipped).unwrap_err(),
            CodeError::Checksum
        );
        // A version 2 body with a valid checksum.
        let mut body = vec![2u8];
        body.extend_from_slice(&[0; HEADER_LEN - 1]);
        let sum = alloy_crypto::hash(&body);
        body.extend_from_slice(&sum[..CHECKSUM_LEN]);
        let wrong = format!("{CODE_PREFIX}{}", base32::encode(&body));
        assert_eq!(LoginCode::decode(&wrong).unwrap_err(), CodeError::Version);
        // Version 1 with no endpoint.
        body[0] = 1;
        body.truncate(HEADER_LEN);
        let sum = alloy_crypto::hash(&body);
        body.extend_from_slice(&sum[..CHECKSUM_LEN]);
        let empty = format!("{CODE_PREFIX}{}", base32::encode(&body));
        assert_eq!(LoginCode::decode(&empty).unwrap_err(), CodeError::Malformed);
    }

    #[test]
    fn a_secret_never_prints() {
        let shown = format!("{:?}", code(vec![v6(1)]));
        assert!(!shown.contains("4, 4"), "{shown}");
    }
}
