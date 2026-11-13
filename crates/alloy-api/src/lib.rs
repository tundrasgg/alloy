//! The native API server and its Rust client (ARFC 0008 sections 2 and 3).
//!
//! The server answers the methods of `alloy-proto`'s services as gRPC over
//! HTTP/2 (clause 3.12) in two places:
//!
//! - the API port, [`ApiPort`]: TLS 1.3 only, sharing one TCP port with the
//!   `stream` underlay by ALPN (clause 3.8), each call authenticated by a
//!   session token in `authorization: Bearer <token>` (clause 3.7.1);
//! - the local admin socket, [`ApiServer::serve_local_admin`]: HTTP/2 by prior
//!   knowledge without TLS, each call a local admin call (clause 3.7.2).
//!
//! It serves `alloy.cluster.v1alpha1.ClusterService` with `GetNode` and
//! `ListNodes` (clause 3.13) over a [`NodeStore`], paginated as clause 3.4
//! says, and authenticates and authorizes each call through an [`Access`].
//! Errors are those of clause 3.6, carried as `alloy-proto`'s
//! [`error`](alloy_proto::error) module says. A path that names no method
//! fails with `UNIMPLEMENTED` and no Alloy code, and a request or response
//! larger than 4 MiB with `InvalidArgument`.
//!
//! Every connection is an `alloy-rt` stream and every task an `alloy-rt`
//! task. TLS uses the `aws-lc-rs` provider as [`tls`] configures it; its
//! tests run under the `os` runtime on loopback (ADR 0042). Under the `mem`
//! `net` implementation the port and its clients exchange the same bytes in
//! the clear instead, as [`Transport`] decides (clause 3.8.1). Each listener
//! bounds its open connections and closes idle ones ([`ConnLimits`]), and no
//! byte the server sends depends on a clock outside `alloy-rt`.
//!
//! [`client`] is the generated client over one connection, with the helpers
//! of `product/principles.md` A18.

#![forbid(unsafe_code)]
#![allow(
    clippy::result_large_err,
    reason = "every fallible path ends in the wire `Error` of ARFC 0008 clause 3.6, returned unchanged; boxing it would only move the copy"
)]

mod access;
pub mod client;
mod io;
mod page;
mod server;
mod service;
mod store;
pub mod tls;
mod transport;

#[cfg(test)]
mod tests;

pub use access::{
    Access, Action, AuthnError, MAX_TOKEN_LEN, Principal, SessionToken, UNAUTHENTICATED_MESSAGE,
    unauthenticated,
};
pub use page::{DEFAULT_PAGE_SIZE, MAX_PAGE_SIZE};
pub use server::{ApiPort, ApiServer, MAX_MESSAGE_BYTES, StreamConn, StreamUnderlay};
pub use store::{MAX_NAME_LEN, NodeName, NodeStore};
pub use transport::{ConnLimits, Transport};
