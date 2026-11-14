//! Cluster state (ARFC 0006): the seams `log-store` and `state-store`, one
//! `openraft` consensus group on them, and the resources a group holds.
//!
//! Each group is a Raft log and a state machine (ADR 0064). On each member the
//! log and vote live in one [`LogStore`] and the applied state in one
//! [`StateStore`], both chosen at run time through the seam registry:
//! [`register_native`] adds the `redb` implementations, which keep one file
//! each in a node-local directory, and [`register_mem`] adds in-memory ones
//! for simulated hosts and tests.
//!
//! [`Group`] runs one `openraft` group over the two stores. Every timer, task,
//! and random draw of `openraft` goes through the [`alloy_rt::Runtime`] the
//! group is opened with. [`Group`] serves the resource operations of ARFC
//! 0006 section 2.1: create, get, list, update with a generation, and delete.
//!
//! On them it serves the kind `Node` of section 2.6 ([`Node`]): it accepts
//! heartbeats ([`Heartbeat`], section 4.3) as clause 3.14 says, holds what
//! they report as soft state in memory, never through the group (section
//! 2.3), and computes each node's [`NodeState`] when it is read (clause
//! 3.15).

#![forbid(unsafe_code)]

use std::sync::Arc;

use alloy_seam::{Factory, ImplName, RegistryBuilder, RegistryError};

mod config;
mod export;
mod group;
mod mem;
mod native;
mod node;
mod nodes;
mod resource;
mod rt;
mod rules;
mod store;

pub use config::{MemStoreConfig, NativeStoreConfig};
pub use group::{Group, GroupError};
pub use node::{
    HEARD_LEASE, Heartbeat, MAX_ADVERTISE, MAX_NODE_NAME, MAX_RELEASE, MAX_STATUS_ADDRESSES,
    NODE_KIND, Node, NodeSpec, NodeState, NodeStatus, NodeView, RecordTerms, Release, Role,
    StatusReply, last_heard, node_state,
};
pub use resource::{MAX_RESOURCE, Resource, ResourceError, Scope, Uid};
pub use store::{
    ApplyBatch, Entry, LogId, LogState, LogStore, LogStoreSeam, MAX_APPEND_BYTES, MAX_BATCH_BYTES,
    MAX_CHUNK, MAX_ENTRY_PAYLOAD, MAX_KEY, MAX_READ_BYTES, MAX_SCAN, MAX_VALUE, NodeId, Op,
    StateInstall, StateSnapshot, StateStore, StateStoreSeam, StoreError, Vote,
};

fn impl_name(name: &str) -> ImplName {
    // "native" and "mem" satisfy the rules of ImplName::new.
    ImplName::new(name).expect("a fixed valid implementation name")
}

/// Registers `native` for both kinds, with their configuration types
/// ([`NativeStoreConfig`]).
///
/// # Errors
///
/// What [`RegistryBuilder::register`] returns, for example
/// [`RegistryError::Duplicate`] when `native` is registered already.
pub fn register_native(builder: &mut RegistryBuilder) -> Result<(), RegistryError> {
    let log: Factory<LogStoreSeam> = Arc::new(|_, config| native::log_factory(config));
    let state: Factory<StateStoreSeam> = Arc::new(|_, config| native::state_factory(config));
    builder.register::<LogStoreSeam>(impl_name("native"), log)?;
    builder.register::<StateStoreSeam>(impl_name("native"), state)
}

/// Registers `mem` for both kinds, with their configuration types
/// ([`MemStoreConfig`]). Each call creates one table of contents shared by
/// the instances its factories build.
///
/// # Errors
///
/// What [`RegistryBuilder::register`] returns, for example
/// [`RegistryError::Duplicate`] when `mem` is registered already.
pub fn register_mem(builder: &mut RegistryBuilder) -> Result<(), RegistryError> {
    let tables = Arc::new(mem::Tables::default());
    let log_tables = Arc::clone(&tables);
    let log: Factory<LogStoreSeam> =
        Arc::new(move |_, config| mem::log_factory(&log_tables, config));
    let state: Factory<StateStoreSeam> =
        Arc::new(move |_, config| mem::state_factory(&tables, config));
    builder.register::<LogStoreSeam>(impl_name("mem"), log)?;
    builder.register::<StateStoreSeam>(impl_name("mem"), state)
}
