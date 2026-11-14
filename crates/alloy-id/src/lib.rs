//! A node's identity in a cluster (ARFC 0002).
//!
//! `alloyd`'s `init` and `join` and the node channel use this crate for
//! everything that involves a key or the data directory's files, and touch
//! neither cryptography nor the file layout themselves. Every primitive comes
//! from `alloy-crypto`, every random byte from the `alloy-rt` randomness
//! trait, and every file operation from the `alloy-rt` `disk-io` seam.
//!
//! # Interface
//!
//! - [`ids`]: [`ClusterId`] with the infrastructure prefix and address of
//!   section 3.4, [`NodeId`], [`ClusterKeyFp`], [`Role`], and [`Roles`].
//! - [`keys`]: generating the cluster key and node identity key (Ed25519),
//!   the node mesh key (X25519), P-256 keys, and the [`AtRestKey`].
//! - [`record`]: [`NodeRecord`], checked on encode and decode, and
//!   [`SignedNodeRecord`], signed by the cluster key over
//!   `alloy/id/node-record/v1` and the record's bytes, verified and accepted
//!   only while unexpired; the `renew` signature of section 3.5.
//! - [`token`]: [`JoinToken`], its `alyt_` string, and every rejection rule
//!   of section 3.2 as a [`TokenError`].
//! - [`login`]: [`LoginCode`], its `alyc_` string, and every rejection rule
//!   of section 3.7 as a [`CodeError`].
//! - [`api`]: the [`ApiCa`] and the API server certificates it issues, with
//!   the fields of section 2.
//! - [`datadir`]: the rules of an Alloy data directory on Linux and Windows,
//!   over facts the operating system reports (section 3.12).
//! - [`store`]: [`DataDir`], the files of section 3.14 written whole and
//!   durably, `secrets` sealed under the at-rest key, and the at-rest key
//!   rotation that survives a crash at any point.
//! - [`proto`]: the Protocol Buffers messages those use.
//!
//! # Secrets
//!
//! Every type that holds a key or a token secret erases it when dropped and
//! shows none of it through `Debug`. The limit of that guarantee is the one
//! `alloy-crypto` states: copies the compiler makes while moving a value are
//! not erased.

#![forbid(unsafe_code)]

pub mod api;
mod base32;
pub mod datadir;
pub mod ids;
pub mod keys;
pub mod login;
pub mod proto;
pub mod record;
pub mod store;
pub mod token;

pub use api::{ApiCa, ApiServerCertificate};
pub use datadir::{DataDirFacts, DataDirProblem, InitAction};
pub use ids::{ClusterId, ClusterKeyFp, NodeId, Role, Roles};
pub use keys::AtRestKey;
pub use login::{CodeError, LoginCode};
pub use record::{NodeRecord, RecordError, SignedNodeRecord};
pub use store::{DataDir, StoreError};
pub use token::{JoinToken, TokenError};

/// Why generating an identity object failed.
#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
pub enum IdError {
    /// The randomness trait failed.
    #[error("random generator: {0}")]
    Rng(#[from] alloy_rt::RngError),
    /// The randomness trait returned 8 P-256 scalars in a row out of range.
    #[error("the random generator returned no valid P-256 scalar in 8 draws")]
    Generator,
    /// A P-256 key did not decode or encode.
    #[error("P-256 key: {0}")]
    Ecdsa(#[from] alloy_crypto::ecdsa::EcdsaError),
    /// A certificate could not be built.
    #[error("certificate: {0}")]
    X509(#[from] alloy_crypto::x509::X509Error),
    /// A validity time is out of range.
    #[error("a certificate validity time is out of range")]
    Time,
}

#[cfg(test)]
mod tests;
