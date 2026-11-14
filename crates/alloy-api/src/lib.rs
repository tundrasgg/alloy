//! The native API server and its Rust client (ARFC 0008 sections 2 and 3).
//!
//! The server answers the methods of `alloy-proto`'s services as gRPC over
//! HTTP/2 (clause 3.12) in two places:
//!
//! - the API port, [`ApiPort`]: TLS 1.3 only, sharing one TCP port with the
//!   `stream` underlay by ALPN (clause 3.8), each call authenticated by a
//!   session token in `authorization: Bearer <token>` or a SigV4 signature
//!   by an access key ([`sigv4`], clause 3.7.1), except `ExchangeLoginCode`,
//!   which its login code alone authenticates, rate limited before the code
//!   is looked up;
//! - the local admin socket, [`ApiServer::serve_local_admin`]: HTTP/2 by prior
//!   knowledge without TLS, served only to a peer the kernel reports as user
//!   id 0, each call a local admin call (clause 3.7.2, ARFC 0007 clause
//!   3.10.2).
//!
//! It serves what M11 rung 2 builds (ARFC 0002 section 3.15):
//! `alloy.cluster.v1alpha1.ClusterService/ListNodes` (clause 3.13) over a
//! [`NodeStore`], paginated as clause 3.4 says, on both listeners;
//! `alloy.iam.v1alpha1.IamService/ExchangeLoginCode` over [`LoginCodes`] on
//! the API port, refused with `PermissionDenied` on the local admin socket;
//! and `IssueLoginCode` on the local admin socket, refused with
//! `PermissionDenied` on the API port. It authenticates and authorizes each
//! call through an [`Access`]. Errors are those of clause 3.6, carried as
//! `alloy-proto`'s [`error`](alloy_proto::error) module says. A path that
//! names no method it serves, `GetNode` included, fails with `UNIMPLEMENTED`
//! and no Alloy code, and a request or response larger than 4 MiB with
//! `InvalidArgument`.
//!
//! Every connection is an `alloy-rt` stream and every task an `alloy-rt`
//! task. TLS comes from `alloy-crypto`, the only crate that depends on
//! `rustls` (`agent/eng/crypto.md` rule 1), as [`tls`] configures it; its
//! tests run under the `os` runtime on loopback (ADR 0042). Under the `mem`
//! `net` implementation the port and its clients exchange the same bytes in
//! the clear instead, as [`Transport`] decides (clause 3.8.1). The API port
//! counts connections with no credential apart from authenticated API
//! connections and `stream` connections, and closes them after a bounded
//! time and a bounded number of rejected calls ([`ConnLimits`]); the local
//! admin socket bounds its connections and closes idle ones
//! ([`AdminLimits`]). No byte the server sends depends on a clock outside
//! `alloy-rt`.
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
mod conn;
mod iam;
mod io;
mod page;
mod server;
mod service;
pub mod sigv4;
mod store;
pub mod tls;
mod transport;

#[cfg(test)]
mod tests;

pub use access::{
    Access, Action, AuthnError, KeyHolder, MAX_TOKEN_LEN, Principal, SessionToken,
    UNAUTHENTICATED_MESSAGE, unauthenticated,
};
pub use iam::{EXCHANGES_IN_ALL, EXCHANGES_PER_SOURCE, Exchange, Issue, LoginCodes};
pub use page::{DEFAULT_PAGE_SIZE, MAX_PAGE_SIZE};
pub use server::{ApiPort, ApiServer, CLOSE_GRACE, MAX_MESSAGE_BYTES, StreamConn, StreamUnderlay};
pub use store::{MAX_NAME_LEN, NodeName, NodeStore};
pub use transport::{AdminLimits, ConnLimits, Transport};
