//! The node channel of ARFC 0002 section 3.13, sans-I/O.
//!
//! - [`proto`]: the `alloy.node.v1alpha1` node messages, with the status
//!   messages of ARFC 0006 section 4.3, decoded and checked in one place.
//! - [`frame`]: the IPv6 and UDP packet that carries one node message on VNI
//!   0 of an AL session, and the receiver's acceptance rules.
//! - [`requester`]: request ids from the handshake counter, resends every
//!   second, one request outstanding, one held per kind, heartbeats first,
//!   and the choice of control node.
//! - [`entries`]: the control node's entry per origin, so that a request is
//!   handled at most once.
//! - [`heartbeat`]: the heartbeat schedule of ARFC 0006 clause 3.13 and the
//!   addresses a heartbeat reports.
//!
//! None of these owns a socket, a timer, or a task: the caller passes the
//! time and carries out what they return.

pub mod entries;
pub mod frame;
pub mod heartbeat;
pub mod proto;
pub mod requester;
