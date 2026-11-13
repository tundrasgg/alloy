//! Identifiers, roles, and infrastructure addresses (ARFC 0002 sections 2,
//! 3.2, and 3.4).

use std::fmt;
use std::net::Ipv6Addr;

use alloy_crypto::al;
use alloy_crypto::ed25519::VerifyingKey;
use alloy_rt::Rng;

use crate::IdError;

/// Writes `bytes` as lower case hexadecimal.
fn write_hex(f: &mut fmt::Formatter<'_>, bytes: &[u8]) -> fmt::Result {
    for byte in bytes {
        write!(f, "{byte:02x}")?;
    }
    Ok(())
}

/// A cluster's id: 128 random bits, drawn once by `init` (section 3.1).
#[derive(Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub struct ClusterId([u8; 16]);

impl ClusterId {
    /// Draws a new id from `rng`.
    ///
    /// # Errors
    ///
    /// [`IdError::Rng`] when the generator fails.
    pub fn generate(rng: &dyn Rng) -> Result<Self, IdError> {
        let mut bytes = [0u8; 16];
        rng.fill(&mut bytes)?;
        Ok(Self(bytes))
    }

    /// The id with these bytes.
    #[must_use]
    pub const fn from_bytes(bytes: [u8; 16]) -> Self {
        Self(bytes)
    }

    /// The 16 bytes of the id.
    #[must_use]
    pub const fn as_bytes(&self) -> &[u8; 16] {
        &self.0
    }

    /// The cluster's infrastructure prefix, a `/48`: `0xfd`, then the first
    /// 5 bytes of the id, then zeros (section 3.4).
    #[must_use]
    pub fn infra_prefix(&self) -> Ipv6Addr {
        let mut octets = [0u8; 16];
        octets[0] = 0xfd;
        octets[1..6].copy_from_slice(&self.0[..5]);
        Ipv6Addr::from(octets)
    }

    /// The infrastructure address of the node whose mesh key is `mesh_pub`:
    /// the prefix, a zero 16-bit subnet, and the first 8 bytes of
    /// `BLAKE3(mesh_pub)` (section 3.4).
    #[must_use]
    pub fn infra_addr(&self, mesh_pub: &al::PublicKey) -> Ipv6Addr {
        let mut octets = self.infra_prefix().octets();
        octets[8..].copy_from_slice(&alloy_crypto::hash(mesh_pub.as_bytes())[..8]);
        Ipv6Addr::from(octets)
    }
}

/// Writes the id as 32 lower case hexadecimal digits, as the API CA's
/// subject names it (section 2).
impl fmt::Display for ClusterId {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write_hex(f, &self.0)
    }
}

impl fmt::Debug for ClusterId {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "ClusterId({self})")
    }
}

/// A node's id: the first 16 bytes of `BLAKE3(identity_pub)` (section 2).
#[derive(Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub struct NodeId([u8; 16]);

impl NodeId {
    /// The id of the node whose identity key is `identity_pub`.
    #[must_use]
    pub fn of(identity_pub: &VerifyingKey) -> Self {
        let mut bytes = [0u8; 16];
        bytes.copy_from_slice(&alloy_crypto::hash(identity_pub.as_bytes())[..16]);
        Self(bytes)
    }

    /// The id with these bytes.
    #[must_use]
    pub const fn from_bytes(bytes: [u8; 16]) -> Self {
        Self(bytes)
    }

    /// The 16 bytes of the id.
    #[must_use]
    pub const fn as_bytes(&self) -> &[u8; 16] {
        &self.0
    }
}

impl fmt::Display for NodeId {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write_hex(f, &self.0)
    }
}

impl fmt::Debug for NodeId {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "NodeId({self})")
    }
}

/// The fingerprint of a cluster key that a join token carries: the first 16
/// bytes of `BLAKE3(cluster_pub)` (section 3.2).
#[derive(Clone, Copy, PartialEq, Eq, Hash)]
pub struct ClusterKeyFp([u8; 16]);

impl ClusterKeyFp {
    /// The fingerprint of `cluster_pub`.
    #[must_use]
    pub fn of(cluster_pub: &VerifyingKey) -> Self {
        let mut bytes = [0u8; 16];
        bytes.copy_from_slice(&alloy_crypto::hash(cluster_pub.as_bytes())[..16]);
        Self(bytes)
    }

    /// The fingerprint with these bytes.
    #[must_use]
    pub const fn from_bytes(bytes: [u8; 16]) -> Self {
        Self(bytes)
    }

    /// The 16 bytes of the fingerprint.
    #[must_use]
    pub const fn as_bytes(&self) -> &[u8; 16] {
        &self.0
    }

    /// Whether `cluster_pub` has this fingerprint.
    /// Both are public, so the comparison need not take constant time.
    #[must_use]
    pub fn matches(&self, cluster_pub: &VerifyingKey) -> bool {
        Self::of(cluster_pub) == *self
    }
}

impl fmt::Debug for ClusterKeyFp {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "ClusterKeyFp(")?;
        write_hex(f, &self.0)?;
        write!(f, ")")
    }
}

/// A node role. The discriminants are the `Role` enum of section 3.13.
#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub enum Role {
    /// `anchor`.
    Anchor = 1,
    /// `control`.
    Control = 2,
    /// `compute`.
    Compute = 3,
    /// `storage`.
    Storage = 4,
}

impl Role {
    /// Every role, in ascending order.
    pub const ALL: [Role; 4] = [Role::Anchor, Role::Control, Role::Compute, Role::Storage];

    /// The role with this `Role` enum value, `None` for 0 or an unknown
    /// value.
    #[must_use]
    pub fn from_proto(value: i32) -> Option<Self> {
        Self::ALL.into_iter().find(|role| *role as i32 == value)
    }

    /// Its bit in a join token's preset roles byte: bit 0 `anchor` to bit 3
    /// `storage` (section 3.2).
    #[must_use]
    pub const fn bit(self) -> u8 {
        1 << (self as u8 - 1)
    }
}

/// A set of roles. Iterates in ascending order, as a node record lists them.
#[derive(Clone, Copy, PartialEq, Eq, Hash, Default)]
pub struct Roles(u8);

impl Roles {
    /// No role, as a client node holds (section 3.11) and as a token without
    /// preset roles carries.
    pub const NONE: Roles = Roles(0);

    /// The set holding `roles`.
    #[must_use]
    pub fn of(roles: &[Role]) -> Self {
        Self(roles.iter().fold(0, |bits, role| bits | role.bit()))
    }

    /// The set a join token's preset roles byte names.
    ///
    /// `None` when any of bits 4 to 7 is set.
    #[must_use]
    pub const fn from_bits(bits: u8) -> Option<Self> {
        if bits & 0xf0 == 0 {
            Some(Self(bits))
        } else {
            None
        }
    }

    /// The preset roles byte of a join token.
    #[must_use]
    pub const fn bits(self) -> u8 {
        self.0
    }

    /// Whether `role` is in the set.
    #[must_use]
    pub const fn contains(self, role: Role) -> bool {
        self.0 & role.bit() != 0
    }

    /// Whether the set is empty.
    #[must_use]
    pub const fn is_empty(self) -> bool {
        self.0 == 0
    }

    /// The roles in ascending order.
    pub fn iter(self) -> impl Iterator<Item = Role> {
        Role::ALL
            .into_iter()
            .filter(move |role| self.contains(*role))
    }
}

impl fmt::Debug for Roles {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_set().entries(self.iter()).finish()
    }
}
