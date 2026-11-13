//! The Protocol Buffers messages of ARFC 0002 that this crate encodes and
//! decodes: the node record and its signed form (section 3.13), and the
//! stored data (section 3.14) with the messages it holds.
//!
//! Field numbers and types are those of `alloy.node.v1alpha1` in the ARFC.
//! A `Role` enum field is declared `int32`, which has the same encoding. These
//! are wire types: [`crate::record::NodeRecord`] is the checked form of a
//! record. Messages that hold secrets erase every field when dropped and show
//! no field through `Debug`.

use std::fmt;

use alloy_crypto::Zeroize;

/// `NodeRecord`.
#[derive(Clone, PartialEq, prost::Message)]
pub struct NodeRecord {
    /// 16 bytes.
    #[prost(bytes = "vec", tag = "1")]
    pub cluster_id: Vec<u8>,
    /// 16 bytes.
    #[prost(bytes = "vec", tag = "2")]
    pub node_id: Vec<u8>,
    /// The node's name.
    #[prost(string, tag = "3")]
    pub name: String,
    /// 32 bytes.
    #[prost(bytes = "vec", tag = "4")]
    pub identity_pub: Vec<u8>,
    /// 32 bytes each, 1 or 2.
    #[prost(bytes = "vec", repeated, tag = "5")]
    pub mesh_pub: Vec<Vec<u8>>,
    /// `Role` values, ascending, no repeats.
    #[prost(int32, repeated, tag = "6")]
    pub roles: Vec<i32>,
    /// The node's zone.
    #[prost(string, tag = "7")]
    pub zone: String,
    /// 16 bytes.
    #[prost(bytes = "vec", tag = "8")]
    pub infra_addr: Vec<u8>,
    /// 4 or 16 bytes each, 0 to 16.
    #[prost(bytes = "vec", repeated, tag = "9")]
    pub advertise: Vec<Vec<u8>>,
    /// The UDP port of the node's AL socket.
    #[prost(uint32, tag = "10")]
    pub mesh_port: u32,
    /// The TCP port of the API and the `stream` underlay.
    #[prost(uint32, tag = "11")]
    pub api_port: u32,
    /// Seconds since 1970-01-01T00:00:00Z.
    #[prost(int64, tag = "12")]
    pub not_after: i64,
    /// Grows with each record issued for the node.
    #[prost(uint64, tag = "13")]
    pub serial: u64,
}

/// `SignedNodeRecord`: the record's bytes as signed, and the 64-byte
/// signature.
#[derive(Clone, PartialEq, prost::Message)]
pub struct SignedNodeRecord {
    /// The encoded `NodeRecord`, kept and forwarded as received.
    #[prost(bytes = "vec", tag = "1")]
    pub record: Vec<u8>,
    /// Ed25519 by the cluster key.
    #[prost(bytes = "vec", tag = "2")]
    pub signature: Vec<u8>,
}

/// `BootstrapView`.
#[derive(Clone, PartialEq, prost::Message)]
pub struct BootstrapView {
    /// The cluster's current public key, 32 bytes.
    #[prost(bytes = "vec", tag = "1")]
    pub cluster_pub: Vec<u8>,
    /// Anchors and control nodes, at most 16.
    #[prost(message, repeated, tag = "2")]
    pub nodes: Vec<SignedNodeRecord>,
}

/// `Disk`.
#[derive(Clone, PartialEq, prost::Message)]
pub struct Disk {
    /// At most 255 bytes.
    #[prost(string, tag = "1")]
    pub path: String,
    /// The disk's size.
    #[prost(uint64, tag = "2")]
    pub size_bytes: u64,
}

/// `Facts`.
#[derive(Clone, PartialEq, prost::Message)]
pub struct Facts {
    /// At most 64 bytes.
    #[prost(string, tag = "1")]
    pub os: String,
    /// At most 32 bytes.
    #[prost(string, tag = "2")]
    pub arch: String,
    /// CPU cores.
    #[prost(uint32, tag = "3")]
    pub cores: u32,
    /// Memory.
    #[prost(uint64, tag = "4")]
    pub memory_bytes: u64,
    /// Whether hardware virtualization is available.
    #[prost(bool, tag = "5")]
    pub virtualization: bool,
    /// At most 32.
    #[prost(message, repeated, tag = "6")]
    pub disks: Vec<Disk>,
}

/// `JoinRequest`. It holds the token's secret, so it erases its fields when
/// dropped and shows none through `Debug`.
#[derive(Clone, PartialEq, prost::Message)]
#[prost(skip_debug)]
pub struct JoinRequest {
    /// 8 bytes.
    #[prost(bytes = "vec", tag = "1")]
    pub token_id: Vec<u8>,
    /// 16 bytes.
    #[prost(bytes = "vec", tag = "2")]
    pub secret: Vec<u8>,
    /// 32 bytes.
    #[prost(bytes = "vec", tag = "3")]
    pub identity_pub: Vec<u8>,
    /// At most 63 bytes.
    #[prost(string, tag = "5")]
    pub name: String,
    /// Whether the name came from the hostname rule.
    #[prost(bool, tag = "6")]
    pub name_is_default: bool,
    /// `Role` values.
    #[prost(int32, repeated, tag = "7")]
    pub roles: Vec<i32>,
    /// At most 63 bytes.
    #[prost(string, tag = "8")]
    pub zone: String,
    /// As in `NodeRecord`.
    #[prost(bytes = "vec", repeated, tag = "9")]
    pub advertise: Vec<Vec<u8>>,
    /// The joiner's AL port.
    #[prost(uint32, tag = "10")]
    pub mesh_port: u32,
    /// The joiner's API port.
    #[prost(uint32, tag = "11")]
    pub api_port: u32,
    /// The machine's facts.
    #[prost(message, optional, tag = "12")]
    pub facts: Option<Facts>,
}

impl Drop for JoinRequest {
    fn drop(&mut self) {
        self.secret.zeroize();
    }
}

impl fmt::Debug for JoinRequest {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("JoinRequest").finish_non_exhaustive()
    }
}

/// `NodeSecrets`, the plaintext of the `secrets` file. It erases every
/// field when dropped and shows none through `Debug`.
#[derive(PartialEq, prost::Message)]
#[prost(skip_debug)]
pub struct NodeSecrets {
    /// Ed25519 seed, 32 bytes.
    #[prost(bytes = "vec", tag = "1")]
    pub identity_key: Vec<u8>,
    /// X25519 private key, 32 bytes.
    #[prost(bytes = "vec", tag = "2")]
    pub mesh_key: Vec<u8>,
    /// During a node key rotation.
    #[prost(bytes = "vec", tag = "3")]
    pub previous_mesh_key: Vec<u8>,
    /// While joining: the token string.
    #[prost(string, tag = "4")]
    pub join_token: String,
    /// While joining: the request of section 3.3 step 6.
    #[prost(message, optional, tag = "5")]
    pub join_request: Option<JoinRequest>,
    /// Control nodes: Ed25519 seed.
    #[prost(bytes = "vec", tag = "6")]
    pub cluster_key: Vec<u8>,
    /// Control nodes, during a rotation overlap.
    #[prost(bytes = "vec", tag = "7")]
    pub previous_cluster_key: Vec<u8>,
    /// Control nodes: PKCS#8 DER.
    #[prost(bytes = "vec", tag = "8")]
    pub api_ca_key: Vec<u8>,
    /// Control nodes: PKCS#8 DER.
    #[prost(bytes = "vec", tag = "9")]
    pub api_server_key: Vec<u8>,
}

impl Drop for NodeSecrets {
    fn drop(&mut self) {
        self.identity_key.zeroize();
        self.mesh_key.zeroize();
        self.previous_mesh_key.zeroize();
        self.join_token.zeroize();
        self.cluster_key.zeroize();
        self.previous_cluster_key.zeroize();
        self.api_ca_key.zeroize();
        self.api_server_key.zeroize();
    }
}

impl fmt::Debug for NodeSecrets {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("NodeSecrets").finish_non_exhaustive()
    }
}

/// `NodeState`, the `node` file: not secret.
#[derive(Clone, PartialEq, prost::Message)]
pub struct NodeState {
    /// 16 bytes.
    #[prost(bytes = "vec", tag = "1")]
    pub cluster_id: Vec<u8>,
    /// Current, then previous during an overlap.
    #[prost(bytes = "vec", repeated, tag = "2")]
    pub cluster_pub: Vec<Vec<u8>>,
    /// Once joined.
    #[prost(message, optional, tag = "3")]
    pub record: Option<SignedNodeRecord>,
    /// Once joined.
    #[prost(message, optional, tag = "4")]
    pub view: Option<BootstrapView>,
    /// A `pending` reply was received.
    #[prost(bool, tag = "5")]
    pub pending: bool,
    /// Control nodes: DER.
    #[prost(bytes = "vec", tag = "6")]
    pub api_ca_certificate: Vec<u8>,
    /// Control nodes: DER.
    #[prost(bytes = "vec", tag = "7")]
    pub api_server_certificate: Vec<u8>,
}
