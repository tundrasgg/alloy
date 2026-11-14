//! The API's view of the control plane: the [`NodeStore`], [`Access`], and
//! [`LoginCodes`] of `alloy-api` over the global group of `alloy-state`, as
//! M11 rung 2 serves them (ARFC 0002 section 3.15, ARFC 0007 section 3.11).
//!
//! With no policy and no role but `cloud-admin` in rung 2, a call is allowed
//! exactly when a local admin's fixed policy (ARFC 0007 clause 3.10.1), or
//! the cluster role `cloud-admin` of the calling user, allows it, and the
//! user and its org are enabled. A session token never authenticates: rung 2
//! issues none (ARFC 0007 section 3.11).

#![allow(
    clippy::result_large_err,
    reason = "every fallible path ends in the wire `Error` of ARFC 0008 clause 3.6, which alloy-api takes unboxed"
)]

use std::fmt;
use std::net::{IpAddr, SocketAddr};
use std::sync::Arc;
use std::time::Duration;

use alloy_api::{
    Access, Action, AuthnError, Exchange, Issue, KeyHolder, LoginCodes, NodeName, NodeStore,
    Principal, SessionToken,
};
use alloy_crypto::Zeroizing;
use alloy_crypto::aead::{AeadKey, NONCE_LEN};
use alloy_proto::cluster::v1alpha1 as pb;
use alloy_proto::common::v1alpha1::{Error, ErrorCode};
use alloy_proto::iam::v1alpha1::{Endpoint, ExchangeLoginCodeResponse, IssueLoginCodeResponse};
use alloy_rt::{BoxFuture, WallTime};
use alloy_state::{
    AccessKeyId, ClusterRole, CodeId, Group, GroupError, IdentityError, IssuedBy, NodeState,
    NodeView, ResourceError, Role, User,
};

/// The associated data label of a sealed access key secret (ARFC 0007
/// section 3.11).
const ACCESS_KEY_AD: &[u8; 27] = b"alloy/at-rest/access-key/v1";

/// The length of an access key secret.
const SECRET_LEN: usize = 40;

/// The pause a client is told to wait when the group cannot answer.
const UNAVAILABLE_RETRY: Duration = Duration::from_secs(1);

/// What the API reads and writes, on this control node: the global group,
/// the node's name, the at-rest key that seals access key secrets, and what
/// a login code names of the cluster.
pub(crate) struct ControlPlane {
    pub(crate) group: GroupSlot,
    /// This control node's name, for `root@<node name>`.
    pub(crate) node: String,
    pub(crate) at_rest: AeadKey,
    pub(crate) cluster_id: [u8; 16],
    pub(crate) api_ca_fp: [u8; 16],
    /// The API endpoint of each control node, in ascending order of node
    /// name, at most 4 (ARFC 0002 section 3.7).
    pub(crate) endpoints: Vec<SocketAddr>,
}

/// The global group while the node serves it. Calls take a handle for their
/// own length, and [`GroupSlot::take`] ends serving: calls that start after
/// it fail with `Unavailable`, so a connection that stays open holds no
/// store.
#[derive(Debug)]
pub(crate) struct GroupSlot(std::sync::RwLock<Option<Arc<Group>>>);

impl GroupSlot {
    pub(crate) fn new(group: Arc<Group>) -> GroupSlot {
        GroupSlot(std::sync::RwLock::new(Some(group)))
    }

    /// The group, or `Unavailable` once the node has stopped serving it.
    fn get(&self) -> Result<Arc<Group>, GroupError> {
        self.0
            .read()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .clone()
            .ok_or_else(|| GroupError::Unavailable(alloy_seam::Detail::new("the node is stopping")))
    }

    /// Ends serving and returns the group.
    pub(crate) fn take(&self) -> Option<Arc<Group>> {
        self.0
            .write()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .take()
    }
}

impl fmt::Debug for ControlPlane {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("ControlPlane")
            .field("node", &self.node)
            .field("endpoints", &self.endpoints)
            .finish_non_exhaustive()
    }
}

/// The API error a failed group operation gives.
pub(crate) fn group_error(error: &GroupError) -> Error {
    match error {
        GroupError::Resource(ResourceError::NotFound) => {
            Error::new(ErrorCode::NotFound, "The resource does not exist.")
        }
        GroupError::Resource(ResourceError::Invalid(detail)) => Error::new(
            ErrorCode::InvalidArgument,
            format!("The request is not valid: {detail}."),
        ),
        GroupError::Resource(ResourceError::Conflict { .. }) => Error::new(
            ErrorCode::Conflict,
            "The resource changed while the call ran, so nothing was changed. Read it again and \
             retry.",
        ),
        GroupError::Resource(other) => Error::new(
            ErrorCode::Conflict,
            format!("The control plane refused the change: {other}."),
        ),
        GroupError::Unavailable(_) => Error::new(
            ErrorCode::Unavailable,
            "The control plane cannot answer now, so the call may or may not have taken effect. \
             Try again shortly.",
        )
        .with_retry_delay(UNAVAILABLE_RETRY),
        GroupError::Store(_) => Error::new(
            ErrorCode::Internal,
            "The control plane could not read its state. Check the log of alloyd on the control \
             node.",
        ),
    }
}

/// The full name of a user, which names it as a principal and as a
/// resource: `orgs/<org>/users/<user>`.
fn user_name(org: &str, user: &str) -> String {
    format!("orgs/{org}/users/{user}")
}

/// The org and user a full user name names.
fn parse_user(name: &str) -> Option<(&str, &str)> {
    let rest = name.strip_prefix("orgs/")?;
    let (org, user) = rest.split_once("/users/")?;
    (!org.is_empty() && !user.is_empty() && !user.contains('/')).then_some((org, user))
}

fn timestamp(time: WallTime) -> prost_types::Timestamp {
    let nanos = time.0.rem_euclid(1_000_000_000);
    prost_types::Timestamp {
        seconds: time.0.div_euclid(1_000_000_000),
        // Below 10^9, so it fits.
        nanos: i32::try_from(nanos).unwrap_or(0),
    }
}

fn role(role: Role) -> pb::Role {
    match role {
        Role::Anchor => pb::Role::Anchor,
        Role::Control => pb::Role::Control,
        Role::Compute => pb::Role::Compute,
        Role::Storage => pb::Role::Storage,
    }
}

/// A node as the API returns it (ARFC 0006 section 2.6, ARFC 0008 clause
/// 3.13).
pub(crate) fn node(view: &NodeView) -> pb::Node {
    let node = &view.node;
    let state = match view.state {
        NodeState::Pending => pb::NodeState::Pending,
        NodeState::Joining => pb::NodeState::Joining,
        NodeState::Active => pb::NodeState::Active,
        NodeState::Unreachable => pb::NodeState::Unreachable,
    };
    let status = &node.status;
    pb::Node {
        name: node.name.clone(),
        uid: node.uid.0.to_vec(),
        generation: i64::try_from(node.generation).unwrap_or(i64::MAX),
        create_time: None,
        labels: node.labels.clone().into_iter().collect(),
        deleting: node.deleting,
        spec: Some(pb::NodeSpec {
            roles: node.spec.roles.iter().map(|r| role(*r).into()).collect(),
            zone: node.spec.zone.clone(),
            advertise: node.spec.advertise.iter().map(IpAddr::to_string).collect(),
        }),
        status: Some(pb::NodeStatus {
            state: state.into(),
            node_id: status.node_id.0.to_vec(),
            record_serial: status.record.map_or(0, |r| r.serial),
            record_not_after: status.record.map(|r| prost_types::Timestamp {
                seconds: r.not_after,
                nanos: 0,
            }),
            reported: status.reported,
            unreachable_since: status.unreachable_since.map(timestamp),
            running_release: status
                .running_release
                .as_ref()
                .map(|r| r.as_str().to_owned())
                .unwrap_or_default(),
            addresses: view.addresses.iter().map(IpAddr::to_string).collect(),
            last_heartbeat_time: view.last_heartbeat_time.map(timestamp),
            mesh: pb::Mesh::Unspecified.into(),
        }),
    }
}

impl ControlPlane {
    /// The user `org`/`user` when it, and its org, are enabled.
    async fn enabled_user(&self, org: &str, user: &str) -> Result<Option<User>, GroupError> {
        let group = self.group.get()?;
        let Some(found) = group.get_user(org, user).await? else {
            return Ok(None);
        };
        let org_enabled = group
            .get_org(org)
            .await?
            .is_some_and(|o| o.enabled && !o.deleting);
        Ok((found.enabled && !found.deleting && org_enabled).then_some(found))
    }

    /// The secret of `key`, unsealed under the at-rest key.
    fn unseal(&self, id: &AccessKeyId, sealed: &[u8]) -> Option<Zeroizing<String>> {
        let (nonce, ciphertext) = sealed.split_at_checked(NONCE_LEN)?;
        let nonce: [u8; NONCE_LEN] = nonce.try_into().ok()?;
        let mut aad = ACCESS_KEY_AD.to_vec();
        aad.extend_from_slice(id.as_str().as_bytes());
        let plain = self.at_rest.open(&nonce, &aad, ciphertext).ok()?;
        if plain.len() != SECRET_LEN {
            return None;
        }
        let text = std::str::from_utf8(&plain).ok()?;
        Some(Zeroizing::new(text.to_owned()))
    }

    async fn find_key(&self, id: &str) -> Result<KeyHolder, AuthnError> {
        let refused = || AuthnError::Refused;
        let id = AccessKeyId::parse(id).ok_or_else(refused)?;
        let failed = |e: GroupError| AuthnError::Failed(group_error(&e));
        let group = self.group.get().map_err(failed)?;
        let key = group
            .get_access_key(&id)
            .await
            .map_err(failed)?
            .ok_or_else(refused)?;
        if key.revoked || key.deleting {
            return Err(refused());
        }
        let users = group.list_users().await.map_err(failed)?;
        let user = users
            .iter()
            .find(|u| u.uid == key.principal && u.org == key.org)
            .ok_or_else(refused)?;
        let secret = self.unseal(&key.id, &key.secret_sealed).ok_or_else(|| {
            AuthnError::Failed(Error::new(
                ErrorCode::Internal,
                "An access key's secret does not open under this control node's at-rest key. \
                     Check the log of alloyd on the control node.",
            ))
        })?;
        Ok(KeyHolder {
            principal: Principal::Authenticated(user_name(&user.org, &user.name)),
            secret,
        })
    }

    async fn check(
        &self,
        principal: &Principal,
        action: Action,
        resource: &str,
    ) -> Result<(), Error> {
        let denied = || {
            Error::new(
                ErrorCode::PermissionDenied,
                format!(
                    "The caller may not take the action {} on {resource}, so the call was refused.",
                    action.as_str()
                ),
            )
            .with_resource(resource)
        };
        let name = match principal {
            // The fixed policy allows every action rung 2 serves (ARFC 0007
            // clause 3.10.1).
            Principal::LocalAdmin => return Ok(()),
            Principal::Authenticated(name) => name,
        };
        let (org, user) = parse_user(name).ok_or_else(denied)?;
        let user_found = self
            .enabled_user(org, user)
            .await
            .map_err(|e| group_error(&e))?
            .ok_or_else(denied)?;
        let cloud_admin = user_found.cluster_roles.contains(&ClusterRole::CloudAdmin);
        let allowed = match action {
            // `cloud-admin` allows `cluster:ListNodes` on every node (ARFC
            // 0007 section 3.11).
            Action::ListNodes => cloud_admin,
            // On the holder's own user only (ARFC 0007 clause 3.9.2).
            Action::IssueLoginCode => cloud_admin && resource == name.as_str(),
        };
        if allowed { Ok(()) } else { Err(denied()) }
    }

    async fn issue_code(&self, issue: Issue) -> Result<IssueLoginCodeResponse, Error> {
        let issued_by = match &issue.principal {
            Principal::LocalAdmin => IssuedBy::LocalAdmin(self.node.clone()),
            // Rung 2 issues login codes only through `init` and the local
            // admin socket (ARFC 0002 section 3.15); the server refuses
            // every other caller first.
            Principal::Authenticated(_) => {
                return Err(Error::new(
                    ErrorCode::PermissionDenied,
                    "Login codes are issued only through the local admin socket, so this call \
                     was refused.",
                ));
            }
        };
        let code = self
            .group
            .get()
            .map_err(|e| group_error(&e))?
            .issue_login_code(&issue.org, &issue.user, issue.secret_hash, issued_by)
            .await
            .map_err(|e| identity_error(&e, &user_name(&issue.org, &issue.user)))?;
        Ok(IssueLoginCodeResponse {
            cluster_id: self.cluster_id.to_vec(),
            api_ca_fp: self.api_ca_fp.to_vec(),
            code_id: code.code_id.0.to_vec(),
            endpoints: self
                .endpoints
                .iter()
                .map(|e| Endpoint {
                    address: match e.ip() {
                        IpAddr::V4(v4) => v4.octets().to_vec(),
                        IpAddr::V6(v6) => v6.octets().to_vec(),
                    },
                    port: u32::from(e.port()),
                })
                .collect(),
            expire_time: Some(timestamp(code.expire_time)),
        })
    }

    async fn exchange_code(&self, exchange: Exchange) -> Result<ExchangeLoginCodeResponse, Error> {
        // A code id or secret of the wrong length names no code.
        let code_id: [u8; 8] = exchange
            .code_id
            .as_slice()
            .try_into()
            .map_err(|_| alloy_api::unauthenticated())?;
        let secret: Zeroizing<[u8; 16]> = Zeroizing::new(
            exchange
                .secret
                .as_slice()
                .try_into()
                .map_err(|_| alloy_api::unauthenticated())?,
        );
        let replaces = (!exchange.replaces.is_empty()).then_some(exchange.replaces.as_str());
        let done = self
            .group
            .get()
            .map_err(|e| group_error(&e))?
            .exchange_login_code(
                CodeId(code_id),
                &secret,
                &exchange.name,
                replaces,
                &self.at_rest,
            )
            .await
            .map_err(|e| identity_error(&e, "login code"))?;
        Ok(ExchangeLoginCodeResponse {
            access_key_id: done.key.id.as_str().to_owned(),
            secret: done.secret.expose().to_owned(),
            name: done.key.key_name.clone(),
            user: done.user.name.clone(),
            org: done.user.org.clone(),
            // Rung 2 holds no project (ARFC 0002 section 3.15).
            project: String::new(),
        })
    }
}

/// The API error of a refused issue or exchange (ARFC 0002 section 3.7).
/// The reason of a spent, voided, or expired code leads its message.
fn identity_error(error: &IdentityError, resource: &str) -> Error {
    let spent = |reason: &str, what: &str| {
        Error::new(
            ErrorCode::PermissionDenied,
            format!("{reason}: the login code {what}, so it was not exchanged. Ask for a new one."),
        )
        .with_next_step(
            "alloy.iam.v1alpha1.IamService/IssueLoginCode",
            "sudo alloy login --issue-code",
        )
    };
    match error {
        IdentityError::Unauthenticated => alloy_api::unauthenticated(),
        IdentityError::LoginCodeUsed => spent("login_code_used", "was already used"),
        IdentityError::LoginCodeVoided => spent("login_code_voided", "was voided"),
        IdentityError::LoginCodeExpired { expire_time } => spent(
            "login_code_expired",
            &format!(
                "expired at {} seconds since 1970",
                expire_time.0.div_euclid(1_000_000_000)
            ),
        ),
        IdentityError::InvalidName(rule) => Error::new(
            ErrorCode::InvalidArgument,
            format!("The key name is not valid, so no key was made and the code is unchanged: it must be {rule}."),
        )
        .with_violation("name", *rule),
        IdentityError::PermissionDenied => Error::new(
            ErrorCode::PermissionDenied,
            "The user or its organization is disabled or gone, so the call was refused.",
        )
        .with_resource(resource),
        IdentityError::LimitExceeded(limit) => Error::new(
            ErrorCode::QuotaExceeded,
            format!("The user holds {limit}, so no key was made. Revoke a key first."),
        ),
        IdentityError::Group(GroupError::Resource(ResourceError::NotFound)) => Error::new(
            ErrorCode::NotFound,
            format!("{resource} does not exist."),
        )
        .with_resource(resource),
        IdentityError::Group(group) => group_error(group),
    }
}

/// The API's handle on the control plane, shared by the three traits.
#[derive(Clone, Debug)]
pub(crate) struct Plane(pub(crate) Arc<ControlPlane>);

impl NodeStore for Plane {
    fn list<'a>(
        &'a self,
        after: Option<&'a NodeName>,
        limit: usize,
    ) -> BoxFuture<'a, Result<Vec<pb::Node>, Error>> {
        Box::pin(async move {
            let group = self.0.group.get().map_err(|e| group_error(&e))?;
            let views = group.list_nodes().await.map_err(|e| group_error(&e))?;
            Ok(views
                .iter()
                .filter(|v| after.is_none_or(|a| v.node.name.as_str() > a.as_str()))
                .take(limit)
                .map(node)
                .collect())
        })
    }
}

impl Access for Plane {
    fn authenticate<'a>(
        &'a self,
        _token: &'a SessionToken,
    ) -> BoxFuture<'a, Result<Principal, AuthnError>> {
        // Rung 2 issues no session token, so none authenticates (ARFC 0007
        // section 3.11).
        Box::pin(async { Err(AuthnError::Refused) })
    }

    fn access_key<'a>(&'a self, id: &'a str) -> BoxFuture<'a, Result<KeyHolder, AuthnError>> {
        Box::pin(self.0.find_key(id))
    }

    fn authorize<'a>(
        &'a self,
        principal: &'a Principal,
        action: Action,
        resource: &'a str,
    ) -> BoxFuture<'a, Result<(), Error>> {
        Box::pin(self.0.check(principal, action, resource))
    }
}

impl LoginCodes for Plane {
    fn issue(&self, issue: Issue) -> BoxFuture<'_, Result<IssueLoginCodeResponse, Error>> {
        Box::pin(self.0.issue_code(issue))
    }

    fn exchange(
        &self,
        exchange: Exchange,
    ) -> BoxFuture<'_, Result<ExchangeLoginCodeResponse, Error>> {
        Box::pin(self.0.exchange_code(exchange))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn user_names_round_trip() {
        assert_eq!(user_name("default", "admin"), "orgs/default/users/admin");
        assert_eq!(
            parse_user("orgs/default/users/admin"),
            Some(("default", "admin"))
        );
        for bad in [
            "orgs//users/a",
            "orgs/a/users/",
            "users/a",
            "orgs/a/users/b/c",
        ] {
            assert_eq!(parse_user(bad), None, "{bad}");
        }
    }

    #[test]
    fn timestamps_split_seconds_and_nanos() {
        let t = timestamp(WallTime(-1));
        assert_eq!((t.seconds, t.nanos), (-1, 999_999_999));
        let t = timestamp(WallTime(1_500_000_001));
        assert_eq!((t.seconds, t.nanos), (1, 500_000_001));
    }
}
