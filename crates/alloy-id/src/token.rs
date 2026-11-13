//! Join tokens (ARFC 0002 section 3.2).

use std::fmt;
use std::net::{IpAddr, Ipv4Addr, Ipv6Addr};

use alloy_crypto::al;
use alloy_crypto::{Zeroize, Zeroizing};
use alloy_rt::Rng;

use crate::ids::{ClusterId, ClusterKeyFp, Roles};
use crate::{IdError, base32};

/// The prefix of every join token string.
pub const TOKEN_PREFIX: &str = "alyt_";

/// The longest join token string, in characters.
pub const MAX_TOKEN_LEN: usize = 512;

/// The most anchors a token lists.
pub const MAX_ANCHORS: usize = 4;

/// The most addresses a token lists per anchor.
pub const MAX_ADDRESSES: usize = 4;

const VERSION: u8 = 1;
const CHECKSUM_LEN: usize = 4;
/// version, cluster_id, cluster_key_fp, token_id, secret, preset roles,
/// anchor count.
const HEADER_LEN: usize = 1 + 16 + 16 + 8 + 16 + 1 + 1;

/// A token's public lookup id.
#[derive(Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash, Debug)]
pub struct TokenId(pub [u8; 8]);

/// A token's 16-byte secret. The control plane stores only its BLAKE3
/// hash. It erases itself when dropped and shows no bytes through `Debug`.
pub struct TokenSecret(Zeroizing<[u8; 16]>);

impl TokenSecret {
    /// Draws a new secret.
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

    /// `BLAKE3(secret)`, what the control plane stores (section 3.14).
    #[must_use]
    pub fn hash(&self) -> [u8; 32] {
        alloy_crypto::hash(&self.0[..])
    }
}

impl fmt::Debug for TokenSecret {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("TokenSecret").finish_non_exhaustive()
    }
}

/// One address of an anchor in a token.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub struct TokenAddress {
    /// The anchor's advertise address.
    pub ip: IpAddr,
    /// The anchor's `mesh_port`.
    pub mesh_port: u16,
    /// The TCP port of its `stream` underlay, its `api_port`, or 0 for none.
    pub stream_port: u16,
}

/// One anchor in a token.
#[derive(Clone, PartialEq, Eq, Debug)]
pub struct TokenAnchor {
    /// The anchor's mesh public key.
    pub mesh_pub: al::PublicKey,
    /// Its addresses, 1 to 4.
    pub addresses: Vec<TokenAddress>,
}

/// A decoded join token.
#[derive(Debug)]
pub struct JoinToken {
    /// The cluster's id.
    pub cluster_id: ClusterId,
    /// The fingerprint of the cluster key.
    pub cluster_key_fp: ClusterKeyFp,
    /// The public lookup id.
    pub token_id: TokenId,
    /// The secret.
    pub secret: TokenSecret,
    /// The preset roles, [`Roles::NONE`] for none.
    pub preset_roles: Roles,
    /// The anchors, 1 to 4.
    pub anchors: Vec<TokenAnchor>,
}

/// Why a string is not a valid join token. Each is a usage error: nothing
/// is sent and no use is counted (section 3.2).
#[derive(Debug, Clone, Copy, PartialEq, Eq, thiserror::Error)]
pub enum TokenError {
    /// It does not start with `alyt_`.
    #[error("a join token starts with `alyt_`")]
    Prefix,
    /// It is longer than 512 characters.
    #[error("a join token is at most 512 characters")]
    TooLong,
    /// The text after `alyt_` is not unpadded lower case base32.
    #[error("the join token is not valid base32")]
    Encoding,
    /// The checksum does not match.
    #[error("the join token's checksum does not match")]
    Checksum,
    /// The version is not 1.
    #[error("the join token's version is not 1")]
    Version,
    /// A count, family, or reserved bit is out of range, or the fields do
    /// not use every byte exactly.
    #[error("the join token's fields are malformed")]
    Malformed,
    /// No anchor with an address fits in a token (encoding only).
    #[error("no anchor with an address fits in a join token")]
    NoAnchor,
}

impl JoinToken {
    /// Encodes the token as its `alyt_` string. It lists the anchors in the
    /// order given, which the caller makes ascending by node name, each with
    /// its first 4 addresses, and leaves out an anchor without addresses,
    /// every anchor after the fourth listed, and every anchor that would make
    /// the string longer than 512 characters.
    ///
    /// # Errors
    ///
    /// [`TokenError::NoAnchor`] when no anchor is listed.
    pub fn encode(&self) -> Result<String, TokenError> {
        let mut anchors: Vec<Vec<u8>> = Vec::new();
        let mut len = HEADER_LEN + CHECKSUM_LEN;
        for anchor in &self.anchors {
            if anchors.len() == MAX_ANCHORS || anchor.addresses.is_empty() {
                continue;
            }
            let entry = encode_anchor(anchor);
            if encoded_len(len + entry.len()) <= MAX_TOKEN_LEN {
                len += entry.len();
                anchors.push(entry);
            }
        }
        let count = u8::try_from(anchors.len()).map_err(|_| TokenError::Malformed)?;
        if count == 0 {
            return Err(TokenError::NoAnchor);
        }
        let mut bytes = Zeroizing::new(Vec::with_capacity(len));
        bytes.push(VERSION);
        bytes.extend_from_slice(self.cluster_id.as_bytes());
        bytes.extend_from_slice(self.cluster_key_fp.as_bytes());
        bytes.extend_from_slice(&self.token_id.0);
        bytes.extend_from_slice(self.secret.as_bytes());
        bytes.push(self.preset_roles.bits());
        bytes.push(count);
        for entry in &anchors {
            bytes.extend_from_slice(entry);
        }
        let checksum = alloy_crypto::hash(&bytes);
        bytes.extend_from_slice(&checksum[..CHECKSUM_LEN]);
        let mut text = base32::encode(&bytes);
        let token = format!("{TOKEN_PREFIX}{text}");
        text.zeroize();
        Ok(token)
    }

    /// Decodes a join token string, checking every rule of section 3.2
    /// before anything is contacted.
    ///
    /// # Errors
    ///
    /// The [`TokenError`] of the first rule broken, in the order prefix,
    /// length, encoding, checksum, version, then fields.
    pub fn decode(text: &str) -> Result<Self, TokenError> {
        let rest = text.strip_prefix(TOKEN_PREFIX).ok_or(TokenError::Prefix)?;
        if text.len() > MAX_TOKEN_LEN {
            return Err(TokenError::TooLong);
        }
        let bytes = Zeroizing::new(base32::decode(rest).ok_or(TokenError::Encoding)?);
        let body_len = bytes
            .len()
            .checked_sub(CHECKSUM_LEN)
            .ok_or(TokenError::Malformed)?;
        let (body, checksum) = bytes.split_at(body_len);
        if alloy_crypto::hash(body)[..CHECKSUM_LEN] != *checksum {
            return Err(TokenError::Checksum);
        }
        let mut reader = Reader(body);
        if reader.take::<1>()?[0] != VERSION {
            return Err(TokenError::Version);
        }
        let cluster_id = ClusterId::from_bytes(reader.take()?);
        let cluster_key_fp = ClusterKeyFp::from_bytes(reader.take()?);
        let token_id = TokenId(reader.take()?);
        let secret = TokenSecret::from_bytes(reader.take()?);
        let preset_roles = Roles::from_bits(reader.take::<1>()?[0]).ok_or(TokenError::Malformed)?;
        let count = usize::from(reader.take::<1>()?[0]);
        if !(1..=MAX_ANCHORS).contains(&count) {
            return Err(TokenError::Malformed);
        }
        let mut anchors = Vec::with_capacity(count);
        for _ in 0..count {
            anchors.push(decode_anchor(&mut reader)?);
        }
        if !reader.0.is_empty() {
            return Err(TokenError::Malformed);
        }
        Ok(Self {
            cluster_id,
            cluster_key_fp,
            token_id,
            secret,
            preset_roles,
            anchors,
        })
    }
}

/// The characters of a token string holding `bytes` bytes.
fn encoded_len(bytes: usize) -> usize {
    TOKEN_PREFIX.len() + (bytes * 8).div_ceil(5)
}

fn encode_anchor(anchor: &TokenAnchor) -> Vec<u8> {
    let addresses = &anchor.addresses[..anchor.addresses.len().min(MAX_ADDRESSES)];
    let mut out = Vec::with_capacity(33 + addresses.len() * 21);
    out.extend_from_slice(anchor.mesh_pub.as_bytes());
    // At most 4 by the slice above.
    out.push(addresses.len() as u8);
    for address in addresses {
        match address.ip {
            IpAddr::V4(ip) => {
                out.push(4);
                out.extend_from_slice(&ip.octets());
            }
            IpAddr::V6(ip) => {
                out.push(6);
                out.extend_from_slice(&ip.octets());
            }
        }
        out.extend_from_slice(&address.mesh_port.to_be_bytes());
        out.extend_from_slice(&address.stream_port.to_be_bytes());
    }
    out
}

fn decode_anchor(reader: &mut Reader<'_>) -> Result<TokenAnchor, TokenError> {
    let mesh_pub = al::PublicKey::from_bytes(reader.take()?);
    let count = usize::from(reader.take::<1>()?[0]);
    if !(1..=MAX_ADDRESSES).contains(&count) {
        return Err(TokenError::Malformed);
    }
    let mut addresses = Vec::with_capacity(count);
    for _ in 0..count {
        let ip = match reader.take::<1>()?[0] {
            4 => IpAddr::V4(Ipv4Addr::from(reader.take::<4>()?)),
            6 => IpAddr::V6(Ipv6Addr::from(reader.take::<16>()?)),
            _ => return Err(TokenError::Malformed),
        };
        let mesh_port = u16::from_be_bytes(reader.take()?);
        let stream_port = u16::from_be_bytes(reader.take()?);
        addresses.push(TokenAddress {
            ip,
            mesh_port,
            stream_port,
        });
    }
    Ok(TokenAnchor {
        mesh_pub,
        addresses,
    })
}

/// Reads fixed-size fields from the front of a byte slice.
struct Reader<'a>(&'a [u8]);

impl Reader<'_> {
    fn take<const N: usize>(&mut self) -> Result<[u8; N], TokenError> {
        let (head, rest) = self
            .0
            .split_first_chunk::<N>()
            .ok_or(TokenError::Malformed)?;
        self.0 = rest;
        Ok(*head)
    }
}
