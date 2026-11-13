//! The `cluster` service at `v1alpha1` (ARFC 0008 clause 3.13).

use std::sync::Arc;

use alloy_proto::cluster::v1alpha1::cluster_service_server::ClusterService;
use alloy_proto::cluster::v1alpha1::{GetNodeRequest, ListNodesRequest, ListNodesResponse, Node};
use alloy_proto::common::v1alpha1::{Error, ErrorCode};
use prost::Message;
use tonic::{Request, Response, Status};

use crate::access::{Access, Action, Principal};
use crate::page::Page;
use crate::server::MAX_MESSAGE_BYTES;
use crate::store::{NodeName, NodeStore};

/// The methods of `alloy.cluster.v1alpha1.ClusterService` over a store.
#[derive(Debug)]
pub(crate) struct ClusterApi {
    pub(crate) store: Arc<dyn NodeStore>,
    pub(crate) access: Arc<dyn Access>,
}

impl ClusterApi {
    /// The principal the server put in the request after authenticating it.
    fn principal<T>(request: &Request<T>) -> Result<Principal, Error> {
        request
            .extensions()
            .get::<Principal>()
            .cloned()
            .ok_or_else(crate::access::unauthenticated)
    }

    async fn get(&self, request: Request<GetNodeRequest>) -> Result<Node, Error> {
        let principal = Self::principal(&request)?;
        let name = NodeName::new(&request.get_ref().name).map_err(|rule| {
            Error::new(
                ErrorCode::InvalidArgument,
                "The node name is not valid, so no node was read. Give the name as `alloy node \
                 list` shows it.",
            )
            .with_violation("name", rule)
        })?;
        let full = name.full_name();
        self.access
            .authorize(&principal, Action::GetNode, &full)
            .await?;
        let node = self.store.get(&name).await?.ok_or_else(|| {
            Error::new(
                ErrorCode::NotFound,
                format!(
                    "No node is named {}. List the nodes to see their names.",
                    name.as_str()
                ),
            )
            .with_resource(full)
            .with_next_step(
                "alloy.cluster.v1alpha1.ClusterService/ListNodes",
                "alloy node list",
            )
        })?;
        fit(
            node,
            "The node's record is larger than 4 MiB, so it was not sent.",
        )
    }

    async fn list(&self, request: Request<ListNodesRequest>) -> Result<ListNodesResponse, Error> {
        let principal = Self::principal(&request)?;
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
    async fn get_node(&self, request: Request<GetNodeRequest>) -> Result<Response<Node>, Status> {
        self.get(request)
            .await
            .map(Response::new)
            .map_err(Error::into_status)
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
