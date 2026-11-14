//! The `cluster` and `iam` services at `v1alpha1` (ARFC 0008 clause 3.13),
//! as far as M11 rung 2 serves them (ARFC 0002 section 3.15).

use std::net::IpAddr;
use std::sync::Arc;

use alloy_proto::cluster::v1alpha1::cluster_service_server::ClusterService;
use alloy_proto::cluster::v1alpha1::{GetNodeRequest, ListNodesRequest, ListNodesResponse, Node};
use alloy_proto::common::v1alpha1::{Error, ErrorCode};
use alloy_proto::iam::v1alpha1::iam_service_server::IamService;
use alloy_proto::iam::v1alpha1::{
    ExchangeLoginCodeRequest, ExchangeLoginCodeResponse, IssueLoginCodeRequest,
    IssueLoginCodeResponse,
};
use prost::Message;
use tonic::{Request, Response, Status};

use crate::access::{Access, Action, Principal};
use crate::iam::{Exchange, Issue, LoginCodes};
use crate::page::Page;
use crate::server::MAX_MESSAGE_BYTES;
use crate::store::NodeStore;

/// Where a call came from, which the server puts in the request: the
/// principal it authenticated, `None` for `ExchangeLoginCode` on the API
/// port, and the source address of the connection, if any.
#[derive(Clone, Debug)]
pub(crate) struct Origin {
    pub(crate) principal: Option<Principal>,
    pub(crate) source: Option<IpAddr>,
}

fn origin<T>(request: &Request<T>) -> Result<Origin, Error> {
    request
        .extensions()
        .get::<Origin>()
        .cloned()
        .ok_or_else(crate::access::unauthenticated)
}

fn principal<T>(request: &Request<T>) -> Result<Principal, Error> {
    origin(request)?
        .principal
        .ok_or_else(crate::access::unauthenticated)
}

/// The methods of `alloy.cluster.v1alpha1.ClusterService` over a store.
#[derive(Debug)]
pub(crate) struct ClusterApi {
    pub(crate) store: Arc<dyn NodeStore>,
    pub(crate) access: Arc<dyn Access>,
}

impl ClusterApi {
    async fn list(&self, request: Request<ListNodesRequest>) -> Result<ListNodesResponse, Error> {
        let principal = principal(&request)?;
        let r = request.get_ref();
        let page = Page::of(r.page_size, &r.page_token, &r.filter, &r.order_by)?;
        self.access
            .authorize(&principal, Action::ListNodes, "nodes")
            .await?;
        // One more than the page, to learn whether another page follows.
        let mut nodes = self.store.list(page.after.as_ref(), page.size + 1).await?;
        let next_page_token = if nodes.len() > page.size {
            nodes.truncate(page.size);
            nodes
                .last()
                .map(|n| page.next_token(&n.name))
                .unwrap_or_default()
        } else {
            String::new()
        };
        fit(
            ListNodesResponse {
                nodes,
                next_page_token,
            },
            "This page of nodes is larger than 4 MiB, so it was not sent. Ask for fewer nodes \
             with a smaller page_size.",
        )
    }
}

/// `response`, or `InvalidArgument` with `message` when it is larger than 4
/// MiB (ARFC 0008 clause 3.12), checked before it is encoded.
fn fit<T: Message>(response: T, message: &str) -> Result<T, Error> {
    if response.encoded_len() > MAX_MESSAGE_BYTES {
        Err(Error::new(ErrorCode::InvalidArgument, message))
    } else {
        Ok(response)
    }
}

#[tonic::async_trait]
impl ClusterService for ClusterApi {
    /// Not served in M11 rung 2 (ARFC 0002 section 3.15). The server never
    /// routes a call here: it answers the path with `UNIMPLEMENTED` itself.
    async fn get_node(&self, _request: Request<GetNodeRequest>) -> Result<Response<Node>, Status> {
        Err(Status::unimplemented(""))
    }

    async fn list_nodes(
        &self,
        request: Request<ListNodesRequest>,
    ) -> Result<Response<ListNodesResponse>, Status> {
        self.list(request)
            .await
            .map(Response::new)
            .map_err(Error::into_status)
    }
}

/// The length of a login code secret's hash.
const SECRET_HASH_LEN: usize = 32;

/// The methods of `alloy.iam.v1alpha1.IamService` over the login codes.
#[derive(Debug)]
pub(crate) struct IamApi {
    pub(crate) codes: Arc<dyn LoginCodes>,
    pub(crate) access: Arc<dyn Access>,
}

/// The user and org a call names, `admin` and `default` when it names none
/// (ARFC 0007 clause 3.10.1).
fn named(user: &str, org: &str) -> (String, String) {
    let user = if user.is_empty() { "admin" } else { user };
    let org = if org.is_empty() { "default" } else { org };
    (user.to_owned(), org.to_owned())
}

impl IamApi {
    async fn issue(
        &self,
        request: Request<IssueLoginCodeRequest>,
    ) -> Result<IssueLoginCodeResponse, Error> {
        let principal = principal(&request)?;
        if principal != Principal::LocalAdmin {
            // Rung 2 issues login codes only through the local admin socket
            // (ARFC 0002 section 3.15).
            return Err(Error::new(
                ErrorCode::PermissionDenied,
                "Login codes are issued only through the local admin socket of a control node, \
                 so this call was refused. Run the command there as root.",
            )
            .with_next_step(
                "alloy.iam.v1alpha1.IamService/IssueLoginCode",
                "sudo alloy login --issue-code",
            ));
        }
        let r = request.into_inner();
        let secret_hash: [u8; SECRET_HASH_LEN] = r.secret_hash.as_slice().try_into().map_err(|_| {
            Error::new(
                ErrorCode::InvalidArgument,
                "The secret hash is not 32 bytes, so no login code was issued.",
            )
            .with_violation("secret_hash", "must be the 32-byte BLAKE3 hash of the secret")
        })?;
        let (user, org) = named(&r.user, &r.org);
        let resource = format!("orgs/{org}/users/{user}");
        self.access
            .authorize(&principal, Action::IssueLoginCode, &resource)
            .await?;
        let issued = self
            .codes
            .issue(Issue {
                principal,
                user,
                org,
                secret_hash,
            })
            .await?;
        fit(
            issued,
            "The login code is larger than 4 MiB, so it was not sent.",
        )
    }

    async fn exchange(
        &self,
        request: Request<ExchangeLoginCodeRequest>,
    ) -> Result<ExchangeLoginCodeResponse, Error> {
        let origin = origin(&request)?;
        if origin.principal.is_some() {
            // The local admin socket does not serve the exchange (ARFC 0007
            // clause 3.10.2).
            return Err(Error::new(
                ErrorCode::PermissionDenied,
                "A login code is exchanged only on the API port, so this call was refused. Run \
                 alloy login with the code as a user, not through the local admin socket.",
            ));
        }
        let r = request.into_inner();
        let exchanged = self
            .codes
            .exchange(Exchange {
                code_id: r.code_id,
                secret: alloy_crypto::Zeroizing::new(r.secret),
                name: r.name,
                replaces: r.replaces,
                source: origin.source,
            })
            .await?;
        fit(
            exchanged,
            "The access key is larger than 4 MiB, so it was not sent.",
        )
    }
}

#[tonic::async_trait]
impl IamService for IamApi {
    async fn issue_login_code(
        &self,
        request: Request<IssueLoginCodeRequest>,
    ) -> Result<Response<IssueLoginCodeResponse>, Status> {
        self.issue(request)
            .await
            .map(Response::new)
            .map_err(Error::into_status)
    }

    async fn exchange_login_code(
        &self,
        request: Request<ExchangeLoginCodeRequest>,
    ) -> Result<Response<ExchangeLoginCodeResponse>, Status> {
        self.exchange(request)
            .await
            .map(Response::new)
            .map_err(Error::into_status)
    }
}
