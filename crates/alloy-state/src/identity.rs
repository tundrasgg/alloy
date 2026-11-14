//! The identity operations of the global group: orgs, users, and join
//! tokens through the resource operations with their checks, and the login
//! codes of ARFC 0002 section 3.7 with the user access keys they yield
//! (ARFC 0007 clauses 3.9.1 to 3.9.5 and 3.9.10, section 3.11).
//!
//! Issuing a code and exchanging one each read several resources and write
//! more than one, in one transaction. Each is proposed as one
//! [`IdentityCommand`], and every member decides it from the state it
//! applies it to, in log order ([`apply`]), so no other write comes between
//! its reads and its writes. That makes the limits of ARFC 0002 section 3.7
//! and ARFC 0007 section 7, the naming of clause 3.9.3, and "at most one of
//! any number of concurrent exchanges succeeds" (clause 3.9.4) hold.

use std::collections::{BTreeMap, BTreeSet};
use std::fmt;

use alloy_crypto::Zeroizing;
use alloy_crypto::aead::AeadKey;
use alloy_rt::WallTime;
use alloy_seam::Detail;

use crate::group::{Group, GroupError, scan_prefix};
use crate::iam::{
    ACCESS_KEY_KIND, AccessKey, AccessKeyId, AccessKeySecret, CODE_LIFETIME_NANOS, ClusterRole,
    CodeId, CodeState, IssuedBy, KEY_NAME_CUT, LOGIN_CODE_KIND, LoginCodeRecord, MAX_KEPT_KEYS,
    MAX_UNREVOKED_KEYS, MAX_UNUSED_CODES, ORG_KIND, Org, USER_KIND, User, check_name,
};
use crate::resource::{
    Dec, Enc, Resource, ResourceError, Scope, Target, Uid, check_size, encode_resource, key,
    scope_prefix,
};
use crate::store::{Op, StateStore, StoreError};
use crate::token::{JOIN_TOKEN_KIND, JoinTokenRecord, JoinTokenSpec, TokenId};

/// How many times an issue or an exchange is proposed again after another
/// write changed what it read, or after its random id was in use.
const ATTEMPTS: usize = 8;

/// The associated data prefix of a sealed access key secret (ARFC 0007
/// section 3.11).
const SEAL_LABEL: &[u8; 27] = b"alloy/at-rest/access-key/v1";

/// Which limit of ARFC 0007 section 7 refused an exchange.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub enum KeyLimit {
    /// 100 unrevoked access keys per principal.
    Unrevoked,
    /// 1,000 kept access keys per user.
    Kept,
}

impl fmt::Display for KeyLimit {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            KeyLimit::Unrevoked => write!(f, "100 unrevoked access keys per principal"),
            KeyLimit::Kept => write!(f, "1,000 kept access keys per user"),
        }
    }
}

/// Why issuing or exchanging a login code failed. No variant carries a
/// secret.
#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
pub enum IdentityError {
    /// The code id names no code, or the secret is wrong: the same error in
    /// both cases (ARFC 0002 section 3.7).
    #[error("the login code is not valid")]
    Unauthenticated,
    /// The code was spent (reason `login_code_used`).
    #[error("the login code was used")]
    LoginCodeUsed,
    /// The code was voided (reason `login_code_voided`).
    #[error("the login code was voided")]
    LoginCodeVoided,
    /// The code has expired (reason `login_code_expired`).
    #[error("the login code expired")]
    LoginCodeExpired {
        /// When it expired.
        expire_time: WallTime,
    },
    /// The key name does not meet ARFC 0007 section 7 (clause 3.9.3).
    #[error("invalid key name: {0}")]
    InvalidName(&'static str),
    /// The user is disabled or gone, or its org is disabled or gone, when
    /// the write commits (ARFC 0007 clauses 3.9.2 and 3.9.4).
    #[error("permission denied")]
    PermissionDenied,
    /// The user holds as many keys as a limit allows (ARFC 0007 section 7).
    #[error("the exchange exceeds the limit of {0}")]
    LimitExceeded(KeyLimit),
    /// The group refused or could not serve the write or a read.
    #[error(transparent)]
    Group(#[from] GroupError),
}

/// Why a member refused an entry, as the group answers its writer.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) enum Refusal {
    Resource(ResourceError),
    Used,
    Voided,
    Expired(WallTime),
    InvalidName(&'static str),
    PermissionDenied,
    Limit(KeyLimit),
}

impl From<ResourceError> for Refusal {
    fn from(error: ResourceError) -> Self {
        Refusal::Resource(error)
    }
}

impl From<Refusal> for IdentityError {
    fn from(refusal: Refusal) -> Self {
        match refusal {
            Refusal::Resource(error) => IdentityError::Group(error.into()),
            Refusal::Used => IdentityError::LoginCodeUsed,
            Refusal::Voided => IdentityError::LoginCodeVoided,
            Refusal::Expired(expire_time) => IdentityError::LoginCodeExpired { expire_time },
            Refusal::InvalidName(rule) => IdentityError::InvalidName(rule),
            Refusal::PermissionDenied => IdentityError::PermissionDenied,
            Refusal::Limit(limit) => IdentityError::LimitExceeded(limit),
        }
    }
}

/// Issues one login code, `code`, for the user named `user` of the code's
/// org, at the leader's wall time `now`.
#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) struct Issue {
    pub(crate) code: Resource,
    pub(crate) user: String,
    pub(crate) now: WallTime,
}

/// Exchanges the login code `code`, found at `generation` with a secret
/// that matched, for a new access key of the user named `user`.
#[derive(Clone, PartialEq, Eq)]
pub(crate) struct Exchange {
    pub(crate) code: Target,
    pub(crate) generation: u64,
    pub(crate) user: String,
    pub(crate) now: WallTime,
    pub(crate) key_uid: Uid,
    pub(crate) key_id: AccessKeyId,
    pub(crate) secret_sealed: Vec<u8>,
    pub(crate) name: String,
    pub(crate) replaces: Option<AccessKeyId>,
}

/// Shows the length of `secret_sealed` and nothing of its bytes (I-22).
impl fmt::Debug for Exchange {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("Exchange")
            .field("code", &self.code)
            .field("generation", &self.generation)
            .field("user", &self.user)
            .field("now", &self.now)
            .field("key_uid", &self.key_uid)
            .field("key_id", &self.key_id)
            .field("secret_sealed_len", &self.secret_sealed.len())
            .field("name", &self.name)
            .field("replaces", &self.replaces)
            .finish()
    }
}

/// A write of identity state that reads more than one resource.
#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) enum IdentityCommand {
    Issue(Issue),
    Exchange(Exchange),
}

fn time_bits(time: WallTime) -> u64 {
    u64::from_be_bytes(time.0.to_be_bytes())
}

fn time_of(bits: u64) -> WallTime {
    WallTime(i64::from_be_bytes(bits.to_be_bytes()))
}

impl IdentityCommand {
    pub(crate) fn encode(&self, enc: &mut Enc) {
        match self {
            IdentityCommand::Issue(issue) => {
                enc.u8(0)
                    .resource(&issue.code)
                    .bytes(issue.user.as_bytes())
                    .u64(time_bits(issue.now));
            }
            IdentityCommand::Exchange(x) => {
                enc.u8(1)
                    .target(&x.code)
                    .u64(x.generation)
                    .bytes(x.user.as_bytes())
                    .u64(time_bits(x.now))
                    .bytes(&x.key_uid.0)
                    .bytes(x.key_id.as_str().as_bytes())
                    .bytes(&x.secret_sealed)
                    .bytes(x.name.as_bytes());
                match &x.replaces {
                    None => enc.u8(0),
                    Some(id) => enc.u8(1).bytes(id.as_str().as_bytes()),
                };
            }
        }
    }

    pub(crate) fn decode(dec: &mut Dec<'_>) -> Option<IdentityCommand> {
        let id = |dec: &mut Dec<'_>| AccessKeyId::parse(&dec.string()?);
        Some(match dec.u8()? {
            0 => IdentityCommand::Issue(Issue {
                code: dec.resource()?,
                user: dec.string()?,
                now: time_of(dec.u64()?),
            }),
            1 => IdentityCommand::Exchange(Exchange {
                code: dec.target()?,
                generation: dec.u64()?,
                user: dec.string()?,
                now: time_of(dec.u64()?),
                key_uid: Uid(dec.bytes()?.try_into().ok()?),
                key_id: id(dec)?,
                secret_sealed: dec.bytes()?.to_vec(),
                name: dec.string()?,
                replaces: match dec.u8()? {
                    0 => None,
                    1 => Some(id(dec)?),
                    _ => return None,
                },
            }),
            _ => return None,
        })
    }

    /// What can be checked before the command is proposed.
    pub(crate) fn check(&self) -> Result<(), ResourceError> {
        match self {
            IdentityCommand::Issue(issue) => {
                key(&issue.code.target())?;
                check_size(&issue.code)?;
                let code = LoginCodeRecord::from_resource(&issue.code)
                    .ok_or(ResourceError::Invalid("an issue must hold a LoginCode"))?;
                if code.state != CodeState::Unused || issue.code.generation != 1 {
                    return Err(ResourceError::Invalid("an issued code is new and unused"));
                }
                check_name(&issue.user)
            }
            IdentityCommand::Exchange(x) => {
                key(&x.code)?;
                check_name(&x.user)?;
                check_name(&x.name)
            }
        }
    }
}

/// Refuses a create or update of an identity kind or a join token that the
/// kind's checks refuse, so the group never holds one that does not decode
/// as its kind. An `AccessKey` is created only by exchanging a login code
/// (ARFC 0007 clause 3.9.1), and a `LoginCode` only by issuing one, so
/// neither is created through the resource operations.
pub(crate) fn check_kind(resource: &Resource, create: bool) -> Result<(), ResourceError> {
    let decodes = match resource.kind.as_str() {
        JOIN_TOKEN_KIND => JoinTokenRecord::from_resource(resource).is_some(),
        ORG_KIND => Org::from_resource(resource).is_some(),
        USER_KIND => User::from_resource(resource).is_some(),
        ACCESS_KEY_KIND if create => {
            return Err(ResourceError::Invalid(
                "an AccessKey is created only by exchanging a login code",
            ));
        }
        LOGIN_CODE_KIND if create => {
            return Err(ResourceError::Invalid(
                "a LoginCode is created only by issuing one",
            ));
        }
        ACCESS_KEY_KIND => AccessKey::from_resource(resource).is_some(),
        LOGIN_CODE_KIND => LoginCodeRecord::from_resource(resource).is_some(),
        _ => true,
    };
    if decodes {
        Ok(())
    } else {
        Err(ResourceError::Invalid(
            "the resource does not hold its kind's fields in its kind's scope",
        ))
    }
}

/// The writes of a decided command and the resource it answers with.
type Decided = (Vec<(Vec<u8>, Option<Resource>)>, Resource);

fn corrupt(what: &str) -> StoreError {
    StoreError::Corrupt(Detail::new(what))
}

async fn read(store: &dyn StateStore, target: &Target) -> Result<Option<Resource>, StoreError> {
    let Ok(key) = key(target) else {
        return Ok(None);
    };
    match store.get(&key).await? {
        None => Ok(None),
        Some(bytes) => crate::resource::decode_resource(&bytes)
            .map(Some)
            .ok_or_else(|| corrupt("a stored resource does not decode")),
    }
}

async fn read_scope(
    store: &dyn StateStore,
    kind: &str,
    scope: &Scope,
) -> Result<Vec<Resource>, StoreError> {
    match scope_prefix(kind, scope) {
        Ok(prefix) => scan_prefix(store, &prefix).await,
        Err(_) => Ok(Vec::new()),
    }
}

fn target(kind: &str, scope: Scope, name: &str) -> Target {
    Target {
        kind: kind.to_owned(),
        scope,
        name: name.to_owned(),
    }
}

/// Whether a resource named `name` of `kind` exists in any org's scope.
async fn in_any_org(store: &dyn StateStore, kind: &str, name: &str) -> Result<bool, StoreError> {
    for org in read_scope(store, ORG_KIND, &Scope::Cluster).await? {
        if read(store, &target(kind, Scope::Org(org.name), name))
            .await?
            .is_some()
        {
            return Ok(true);
        }
    }
    Ok(false)
}

/// Decides `command` against the state in `store`, every member alike:
/// the ops to apply with the entry and the answer to its writer.
///
/// # Errors
///
/// A [`StoreError`] when a read fails or a stored resource does not decode.
pub(crate) async fn apply(
    store: &dyn StateStore,
    command: IdentityCommand,
) -> Result<(Vec<Op>, Result<Option<Resource>, Refusal>), StoreError> {
    let decided = match &command {
        IdentityCommand::Issue(issue) => {
            let org = match &issue.code.scope {
                Scope::Org(org) => org.clone(),
                _ => return Ok((Vec::new(), Err(Refusal::Resource(ResourceError::NotFound)))),
            };
            let scope = Scope::Org(org.clone());
            let user = read(store, &target(USER_KIND, scope.clone(), &issue.user)).await?;
            let org_found = read(store, &target(ORG_KIND, Scope::Cluster, &org)).await?;
            let codes = read_scope(store, LOGIN_CODE_KIND, &scope).await?;
            let taken = in_any_org(store, LOGIN_CODE_KIND, &issue.code.name).await?;
            decide_issue(issue, user, org_found, &codes, taken)
        }
        IdentityCommand::Exchange(x) => {
            let org = match &x.code.scope {
                Scope::Org(org) => org.clone(),
                _ => return Ok((Vec::new(), Err(Refusal::Resource(ResourceError::NotFound)))),
            };
            let scope = Scope::Org(org.clone());
            let code = read(store, &x.code).await?;
            let user = read(store, &target(USER_KIND, scope.clone(), &x.user)).await?;
            let org_found = read(store, &target(ORG_KIND, Scope::Cluster, &org)).await?;
            let keys = read_scope(store, ACCESS_KEY_KIND, &scope).await?;
            let taken = in_any_org(store, ACCESS_KEY_KIND, &x.key_id.resource_name()).await?;
            decide_exchange(x, code, user, org_found, &keys, taken)
        }
    };
    let decided = decided.and_then(|(writes, answer)| {
        for (_, value) in &writes {
            if let Some(resource) = value {
                check_size(resource)?;
            }
        }
        Ok((writes, answer))
    });
    Ok(match decided {
        Ok((writes, answer)) => {
            let ops = writes
                .into_iter()
                .map(|(key, value)| match value {
                    Some(resource) => Op::Put {
                        key,
                        value: encode_resource(&resource),
                    },
                    None => Op::Delete { key },
                })
                .collect();
            (ops, Ok(Some(answer)))
        }
        Err(refusal) => (Vec::new(), Err(refusal)),
    })
}

/// The user and org a write for `user` of `org` may act for when it
/// commits, or why not: the user must exist with `uid` when given, and
/// both must be enabled and not being deleted.
fn enabled(
    user: Option<Resource>,
    org: Option<Resource>,
    uid: Option<Uid>,
) -> Result<User, Refusal> {
    let user = user
        .as_ref()
        .and_then(User::from_resource)
        .filter(|u| !u.deleting && uid.is_none_or(|uid| u.uid == uid))
        .ok_or(Refusal::PermissionDenied)?;
    let org = org
        .as_ref()
        .and_then(Org::from_resource)
        .ok_or(Refusal::PermissionDenied)?;
    if !user.enabled || !org.enabled || org.deleting {
        return Err(Refusal::PermissionDenied);
    }
    Ok(user)
}

/// Decides an issue (ARFC 0002 section 3.7, ARFC 0007 clause 3.9.2): the
/// user must exist and it and its org be enabled when it commits; a user
/// holding 10 unused, unexpired codes loses the oldest of them.
fn decide_issue(
    issue: &Issue,
    user: Option<Resource>,
    org: Option<Resource>,
    codes: &[Resource],
    taken: bool,
) -> Result<Decided, Refusal> {
    let code = LoginCodeRecord::from_resource(&issue.code)
        .ok_or(ResourceError::Invalid("an issue must hold a LoginCode"))?;
    if user.is_none() {
        return Err(ResourceError::NotFound.into());
    }
    let user = enabled(user, org, None)?;
    if user.uid != code.user {
        // The user was created again since the leader read it.
        return Err(ResourceError::NotFound.into());
    }
    if taken {
        return Err(ResourceError::AlreadyExists.into());
    }
    let mut unused = Vec::new();
    for resource in codes {
        let held = LoginCodeRecord::from_resource(resource).ok_or(ResourceError::Invalid(
            "a stored login code does not decode",
        ))?;
        if held.user == user.uid && held.state == CodeState::Unused && issue.now < held.expire_time
        {
            unused.push((held.expire_time, resource.name.clone(), resource.target()));
        }
    }
    unused.sort_by(|a, b| (a.0, &a.1).cmp(&(b.0, &b.1)));
    let mut writes = Vec::new();
    let over = (unused.len() + 1).saturating_sub(MAX_UNUSED_CODES);
    for (_, _, oldest) in unused.into_iter().take(over) {
        writes.push((key(&oldest)?, None));
    }
    writes.push((key(&issue.code.target())?, Some(issue.code.clone())));
    Ok((writes, issue.code.clone()))
}

/// The name a new key of a user gets (ARFC 0007 clause 3.9.3): `name` when
/// no unrevoked key of the user has it, otherwise `<name>-<n>` with `name`
/// cut to 59 characters and the lowest free `n` from 2.
fn free_name(name: &str, taken: &BTreeSet<&str>) -> Option<String> {
    if !taken.contains(name) {
        return Some(name.to_owned());
    }
    // A name is ASCII (check_name), so a byte cut is a character cut.
    let cut = &name[..name.len().min(KEY_NAME_CUT)];
    // At most MAX_UNREVOKED_KEYS names are taken, so one of these is free.
    (2..=MAX_UNREVOKED_KEYS + 2)
        .map(|n| format!("{cut}-{n}"))
        .find(|candidate| !taken.contains(candidate.as_str()))
}

/// Decides an exchange (ARFC 0002 section 3.7, ARFC 0007 clauses 3.9.3,
/// 3.9.4, 3.9.10, section 7): the code must be as the leader found it and
/// usable, the name valid, the user and org enabled, and the user under
/// both key limits after the key `replaces` names is revoked. Then it
/// spends the code and creates the key, in one transaction.
fn decide_exchange(
    x: &Exchange,
    code: Option<Resource>,
    user: Option<Resource>,
    org: Option<Resource>,
    keys: &[Resource],
    taken: bool,
) -> Result<Decided, Refusal> {
    let found = code.ok_or(ResourceError::NotFound)?;
    if found.generation != x.generation {
        return Err(ResourceError::Conflict {
            named: x.generation,
            current: found.generation,
        }
        .into());
    }
    let mut code = LoginCodeRecord::from_resource(&found).ok_or(ResourceError::Invalid(
        "a stored login code does not decode",
    ))?;
    match code.state {
        CodeState::Spent => return Err(Refusal::Used),
        CodeState::Voided => return Err(Refusal::Voided),
        CodeState::Unused if code.deleting => return Err(Refusal::Voided),
        CodeState::Unused => {}
    }
    if x.now >= code.expire_time {
        return Err(Refusal::Expired(code.expire_time));
    }
    check_name(&x.name).map_err(|_| {
        Refusal::InvalidName("1 to 63 characters of a-z, 0-9, and -, starting with a letter")
    })?;
    let user = enabled(user, org, Some(code.user))?;
    let mut writes = Vec::new();
    let mut held = Vec::new();
    for resource in keys {
        let key = AccessKey::from_resource(resource).ok_or(ResourceError::Invalid(
            "a stored access key does not decode",
        ))?;
        if key.principal != user.uid {
            continue;
        }
        // Clause 3.9.10. In rung 2 no key descends from another (IssuedBy),
        // so the replaced key is deleted by its own revocation (clause
        // 3.9.11) and voids no login code (clause 3.9.7).
        if !key.revoked && x.replaces.as_ref() == Some(&key.id) {
            writes.push((crate::resource::key(&resource.target())?, None));
            continue;
        }
        held.push(key);
    }
    if held.iter().filter(|k| !k.revoked).count() >= MAX_UNREVOKED_KEYS {
        return Err(Refusal::Limit(KeyLimit::Unrevoked));
    }
    if held.len() >= MAX_KEPT_KEYS {
        return Err(Refusal::Limit(KeyLimit::Kept));
    }
    let names = held
        .iter()
        .filter(|k| !k.revoked)
        .map(|k| k.key_name.as_str())
        .collect::<BTreeSet<_>>();
    let key_name = free_name(&x.name, &names)
        .ok_or(ResourceError::Invalid("no free key name within the limit"))?;
    if taken {
        return Err(ResourceError::AlreadyExists.into());
    }
    let created = AccessKey {
        org: code.org.clone(),
        id: x.key_id.clone(),
        uid: x.key_uid,
        generation: 1,
        labels: BTreeMap::new(),
        deleting: false,
        principal: user.uid,
        key_name,
        secret_sealed: x.secret_sealed.clone(),
        issued_by: code.issued_by.clone(),
        revoked: false,
    }
    .to_resource();
    code.state = CodeState::Spent;
    code.generation += 1;
    writes.push((key(&x.code)?, Some(code.to_resource())));
    writes.push((key(&created.target())?, Some(created.clone())));
    Ok((writes, created))
}

/// What a successful exchange returns (ARFC 0002 section 3.7): the new key
/// with the name it got, its secret, which no later call returns (I-22),
/// and the user, whose name and org the response carries. Rung 2 holds no
/// project, so the response's `project` is empty.
#[derive(Debug)]
pub struct Exchanged {
    /// The new key.
    pub key: AccessKey,
    /// Its secret.
    pub secret: AccessKeySecret,
    /// Its user.
    pub user: User,
}

fn decode<T>(found: &Resource, from: fn(&Resource) -> Option<T>) -> Result<T, GroupError> {
    from(found).ok_or_else(|| corrupt("a stored resource does not decode as its kind").into())
}

/// Compares two hashes without an early exit. They are `BLAKE3` digests of
/// secrets, not secrets: what their timing could show is how much of the
/// stored digest a guess's digest shares, which gives no preimage, and a
/// wrong guess spends the code (ARFC 0002 section 3.7).
fn same_hash(a: &[u8; 32], b: &[u8; 32]) -> bool {
    let diff = a.iter().zip(b).fold(0u8, |acc, (x, y)| acc | (x ^ y));
    std::hint::black_box(diff) == 0
}

impl Group {
    fn random<const N: usize>(&self) -> Result<Zeroizing<[u8; N]>, GroupError> {
        let mut bytes = Zeroizing::new([0u8; N]);
        self.scope
            .runtime
            .rng()
            .fill(bytes.as_mut())
            .map_err(|e| GroupError::Unavailable(Detail::new(&e.to_string())))?;
        Ok(bytes)
    }

    fn wall(&self) -> WallTime {
        self.scope.runtime.clock().wall()
    }

    /// Creates the org `name`, enabled or not.
    ///
    /// # Errors
    ///
    /// [`ResourceError::Invalid`] for a name out of ARFC 0007 section 7, and
    /// as [`Group::create`].
    pub async fn create_org(&self, name: &str, enabled: bool) -> Result<Org, GroupError> {
        check_name(name)?;
        let draft = Org {
            name: name.to_owned(),
            uid: Uid([0; 16]),
            generation: 1,
            labels: BTreeMap::new(),
            deleting: false,
            enabled,
        }
        .to_resource();
        let created = self
            .create(ORG_KIND, Scope::Cluster, name, draft.spec, BTreeMap::new())
            .await?;
        decode(&created, Org::from_resource)
    }

    /// Reads the org `name`, linearizably.
    ///
    /// # Errors
    ///
    /// [`GroupError::Store`] for a stored org that does not decode, and as
    /// [`Group::get`].
    pub async fn get_org(&self, name: &str) -> Result<Option<Org>, GroupError> {
        match self.get(ORG_KIND, &Scope::Cluster, name).await? {
            None => Ok(None),
            Some(found) => decode(&found, Org::from_resource).map(Some),
        }
    }

    /// Reads every org, linearizably, in name order.
    ///
    /// # Errors
    ///
    /// As [`Group::get_org`].
    pub async fn list_orgs(&self) -> Result<Vec<Org>, GroupError> {
        self.list(ORG_KIND)
            .await?
            .iter()
            .map(|r| decode(r, Org::from_resource))
            .collect()
    }

    /// Creates the user `name` in the org `org`, enabled or not, with
    /// `cluster_roles`.
    ///
    /// # Errors
    ///
    /// [`ResourceError::Invalid`] for an org or user name out of ARFC 0007
    /// section 7, and as [`Group::create`].
    pub async fn create_user(
        &self,
        org: &str,
        name: &str,
        enabled: bool,
        cluster_roles: BTreeSet<ClusterRole>,
    ) -> Result<User, GroupError> {
        check_name(org)?;
        check_name(name)?;
        let draft = User {
            org: org.to_owned(),
            name: name.to_owned(),
            uid: Uid([0; 16]),
            generation: 1,
            labels: BTreeMap::new(),
            deleting: false,
            enabled,
            cluster_roles,
        }
        .to_resource();
        let created = self
            .create(USER_KIND, draft.scope, name, draft.spec, BTreeMap::new())
            .await?;
        decode(&created, User::from_resource)
    }

    /// Reads the user `name` of the org `org`, linearizably.
    ///
    /// # Errors
    ///
    /// [`GroupError::Store`] for a stored user that does not decode, and as
    /// [`Group::get`].
    pub async fn get_user(&self, org: &str, name: &str) -> Result<Option<User>, GroupError> {
        match self
            .get(USER_KIND, &Scope::Org(org.to_owned()), name)
            .await?
        {
            None => Ok(None),
            Some(found) => decode(&found, User::from_resource).map(Some),
        }
    }

    /// Reads every user, linearizably, by org name, then by name.
    ///
    /// # Errors
    ///
    /// As [`Group::get_user`].
    pub async fn list_users(&self) -> Result<Vec<User>, GroupError> {
        self.list(USER_KIND)
            .await?
            .iter()
            .map(|r| decode(r, User::from_resource))
            .collect()
    }

    /// Creates a join token with no use counted (ARFC 0006 section 2.9).
    ///
    /// # Errors
    ///
    /// [`ResourceError::Invalid`] for a preset zone over 63 bytes,
    /// [`ResourceError::AlreadyExists`] for a `token_id` in use, and as
    /// [`Group::create`].
    pub async fn create_join_token(
        &self,
        spec: JoinTokenSpec,
    ) -> Result<JoinTokenRecord, GroupError> {
        spec.check()?;
        let name = spec.token_id.resource_name();
        let encoded = crate::token::encode_spec(&spec);
        let target = target(JOIN_TOKEN_KIND, Scope::Cluster, &name);
        let status = JoinTokenRecord {
            uid: Uid([0; 16]),
            generation: 1,
            labels: BTreeMap::new(),
            deleting: false,
            spec,
            uses: 0,
        }
        .to_resource()
        .status;
        let created = self
            .create_with_status(target, encoded, status, BTreeMap::new())
            .await?;
        decode(&created, JoinTokenRecord::from_resource)
    }

    /// Reads the join token with id `token_id`, linearizably.
    ///
    /// # Errors
    ///
    /// [`GroupError::Store`] for a stored token that does not decode, and
    /// as [`Group::get`].
    pub async fn get_join_token(
        &self,
        token_id: TokenId,
    ) -> Result<Option<JoinTokenRecord>, GroupError> {
        let name = token_id.resource_name();
        match self.get(JOIN_TOKEN_KIND, &Scope::Cluster, &name).await? {
            None => Ok(None),
            Some(found) => decode(&found, JoinTokenRecord::from_resource).map(Some),
        }
    }

    /// Reads every join token, linearizably, in `token_id` order.
    ///
    /// # Errors
    ///
    /// As [`Group::get_join_token`].
    pub async fn list_join_tokens(&self) -> Result<Vec<JoinTokenRecord>, GroupError> {
        self.list(JOIN_TOKEN_KIND)
            .await?
            .iter()
            .map(|r| decode(r, JoinTokenRecord::from_resource))
            .collect()
    }

    /// Replaces the spec, uses, and labels of the join token
    /// `token.spec.token_id` when its generation is `token.generation`, and
    /// returns it with its generation one more (clause 3.5).
    ///
    /// # Errors
    ///
    /// [`ResourceError::Invalid`] as for [`Group::create_join_token`], and
    /// as [`Group::update`].
    pub async fn update_join_token(
        &self,
        token: &JoinTokenRecord,
    ) -> Result<JoinTokenRecord, GroupError> {
        token.spec.check()?;
        let updated = self.update(token.to_resource()).await?;
        decode(&updated, JoinTokenRecord::from_resource)
    }

    /// Finds a resource of `kind` named `name` in any org's scope, reading
    /// the state as of one linearizable read.
    async fn find_in_orgs(&self, kind: &str, name: &str) -> Result<Option<Resource>, GroupError> {
        self.linearize().await?;
        let store = self.state_store();
        for org in read_scope(store, ORG_KIND, &Scope::Cluster).await? {
            if let Some(found) = read(store, &target(kind, Scope::Org(org.name), name)).await? {
                return Ok(Some(found));
            }
        }
        Ok(None)
    }

    /// Reads the login code with id `code_id`, linearizably, whatever org
    /// it is in.
    ///
    /// # Errors
    ///
    /// [`GroupError::Store`] for a stored code that does not decode, and
    /// [`GroupError::Unavailable`].
    pub async fn get_login_code(
        &self,
        code_id: CodeId,
    ) -> Result<Option<LoginCodeRecord>, GroupError> {
        match self
            .find_in_orgs(LOGIN_CODE_KIND, &code_id.resource_name())
            .await?
        {
            None => Ok(None),
            Some(found) => decode(&found, LoginCodeRecord::from_resource).map(Some),
        }
    }

    /// Reads every login code, linearizably, by org name, then by name.
    ///
    /// # Errors
    ///
    /// As [`Group::get_login_code`].
    pub async fn list_login_codes(&self) -> Result<Vec<LoginCodeRecord>, GroupError> {
        self.list(LOGIN_CODE_KIND)
            .await?
            .iter()
            .map(|r| decode(r, LoginCodeRecord::from_resource))
            .collect()
    }

    /// Reads the access key `id`, linearizably, whatever org it is in.
    ///
    /// # Errors
    ///
    /// As [`Group::get_login_code`].
    pub async fn get_access_key(&self, id: &AccessKeyId) -> Result<Option<AccessKey>, GroupError> {
        match self
            .find_in_orgs(ACCESS_KEY_KIND, &id.resource_name())
            .await?
        {
            None => Ok(None),
            Some(found) => decode(&found, AccessKey::from_resource).map(Some),
        }
    }

    /// Reads every access key, linearizably, by org name, then by id.
    ///
    /// # Errors
    ///
    /// As [`Group::get_login_code`].
    pub async fn list_access_keys(&self) -> Result<Vec<AccessKey>, GroupError> {
        self.list(ACCESS_KEY_KIND)
            .await?
            .iter()
            .map(|r| decode(r, AccessKey::from_resource))
            .collect()
    }

    /// Issues a login code for the user `user` of the org `org` (ARFC 0002
    /// section 3.7): draws its `code_id`, records `secret_hash`, which is
    /// `BLAKE3` of the secret the client generated, `issued_by`, and an
    /// expiry 24 hours after the leader's wall time now. When the user
    /// already holds 10 unused, unexpired codes, the same transaction
    /// deletes the oldest. Who may issue is the caller's to check (ARFC
    /// 0007 clause 3.9.2); this checks what that clause reads at commit.
    ///
    /// # Errors
    ///
    /// [`IdentityError::PermissionDenied`] when, at commit, the user or its
    /// org is disabled or the org is gone; [`ResourceError::NotFound`] when
    /// the user is gone; [`ResourceError::Invalid`] for a name out of ARFC
    /// 0007 section 7; and [`GroupError::Unavailable`], also when 8 draws of
    /// `code_id` in a row were in use or other writes kept changing the
    /// user.
    pub async fn issue_login_code(
        &self,
        org: &str,
        user: &str,
        secret_hash: [u8; 32],
        issued_by: IssuedBy,
    ) -> Result<LoginCodeRecord, IdentityError> {
        check_name(org).map_err(GroupError::from)?;
        check_name(user).map_err(GroupError::from)?;
        for _ in 0..ATTEMPTS {
            let holder = self
                .get_user(org, user)
                .await?
                .ok_or(GroupError::Resource(ResourceError::NotFound))?;
            let now = self.wall();
            let expire_time = now
                .0
                .checked_add(CODE_LIFETIME_NANOS)
                .map(WallTime)
                .ok_or_else(|| {
                    GroupError::Unavailable(Detail::new("the wall clock is too late"))
                })?;
            let code = LoginCodeRecord {
                org: org.to_owned(),
                code_id: CodeId(*self.random::<8>()?),
                uid: Uid(*self.random::<16>()?),
                generation: 1,
                labels: BTreeMap::new(),
                deleting: false,
                user: holder.uid,
                secret_hash,
                issued_by: issued_by.clone(),
                expire_time,
                state: CodeState::Unused,
            };
            let command = IdentityCommand::Issue(Issue {
                code: code.to_resource(),
                user: user.to_owned(),
                now,
            });
            match self.propose_identity(command).await? {
                Ok(Some(created)) => return Ok(decode(&created, LoginCodeRecord::from_resource)?),
                Ok(None) => {
                    return Err(GroupError::Unavailable(Detail::new("no login code")).into());
                }
                // The id was in use, or the user was created again.
                Err(Refusal::Resource(ResourceError::AlreadyExists | ResourceError::NotFound)) => {}
                Err(refusal) => return Err(refusal.into()),
            }
        }
        Err(GroupError::Unavailable(Detail::new("the issue did not commit in 8 attempts")).into())
    }

    /// Exchanges a login code for a new access key of its user (ARFC 0002
    /// section 3.7, ARFC 0007 clauses 3.9.3, 3.9.4, and 3.9.10).
    ///
    /// The code is `code_id` with `secret`, the 16 bytes the `alyc_` string
    /// carries. An unknown `code_id` and a wrong secret fail alike, and a
    /// wrong secret spends an unused code. Then it fails, leaving the code
    /// as it was, when the code is spent, voided, or expired, `name` does
    /// not meet ARFC 0007 section 7, the user or its org is disabled or
    /// gone, or the user holds as many keys as a limit allows, counted
    /// after revoking the key `replaces` names when that is an unrevoked
    /// key of the user (any other `replaces` is ignored). Otherwise one
    /// transaction revokes and deletes that key, spends the code, and
    /// creates the key: a new id, a new secret sealed under `at_rest` as
    /// ARFC 0007 section 3.11 says, `issued_by` copied from the code, and
    /// `name`, or `<name>-<n>` when an unrevoked key of the user has it. Of
    /// any number of concurrent exchanges of one code, at most one
    /// succeeds.
    ///
    /// # Errors
    ///
    /// The [`IdentityError`] of each case above, and
    /// [`GroupError::Unavailable`], also when 8 attempts in a row were
    /// overtaken by other writes or drew an id in use.
    pub async fn exchange_login_code(
        &self,
        code_id: CodeId,
        secret: &[u8; 16],
        name: &str,
        replaces: Option<&str>,
        at_rest: &AeadKey,
    ) -> Result<Exchanged, IdentityError> {
        let replaces = replaces.and_then(AccessKeyId::parse);
        let hash = alloy_crypto::hash(secret);
        for _ in 0..ATTEMPTS {
            let found = self
                .find_in_orgs(LOGIN_CODE_KIND, &code_id.resource_name())
                .await?;
            let Some(found) = found else {
                return Err(IdentityError::Unauthenticated);
            };
            let code = decode(&found, LoginCodeRecord::from_resource)?;
            if !same_hash(&hash, &code.secret_hash) {
                if code.state != CodeState::Unused {
                    return Err(IdentityError::Unauthenticated);
                }
                let mut spent = code.clone();
                spent.state = CodeState::Spent;
                match self.update(spent.to_resource()).await {
                    Err(GroupError::Resource(ResourceError::Conflict { .. })) => continue,
                    Ok(_) | Err(GroupError::Resource(_)) => {
                        return Err(IdentityError::Unauthenticated);
                    }
                    Err(other) => return Err(other.into()),
                }
            }
            let now = self.wall();
            match code.state {
                CodeState::Spent => return Err(IdentityError::LoginCodeUsed),
                CodeState::Voided => return Err(IdentityError::LoginCodeVoided),
                CodeState::Unused => {}
            }
            if now >= code.expire_time {
                return Err(IdentityError::LoginCodeExpired {
                    expire_time: code.expire_time,
                });
            }
            check_name(name).map_err(|_| {
                IdentityError::InvalidName(
                    "1 to 63 characters of a-z, 0-9, and -, starting with a letter",
                )
            })?;
            let users = read_scope(self.state_store(), USER_KIND, &found.scope)
                .await
                .map_err(GroupError::from)?;
            let Some(user) = users
                .iter()
                .filter_map(User::from_resource)
                .find(|u| u.uid == code.user)
            else {
                return Err(IdentityError::PermissionDenied);
            };
            let key_id = AccessKeyId::from_random(&*self.random::<10>()?);
            let key_secret = AccessKeySecret::from_random(&*self.random::<30>()?);
            let nonce = self.random::<24>()?;
            let mut aad = SEAL_LABEL.to_vec();
            aad.extend_from_slice(key_id.as_str().as_bytes());
            let sealed = at_rest
                .seal(&nonce, &aad, key_secret.expose().as_bytes())
                .map_err(|_| GroupError::Unavailable(Detail::new("sealing the secret failed")))?;
            let mut secret_sealed = nonce.to_vec();
            secret_sealed.extend_from_slice(&sealed);
            let command = IdentityCommand::Exchange(Exchange {
                code: found.target(),
                generation: found.generation,
                user: user.name.clone(),
                now,
                key_uid: Uid(*self.random::<16>()?),
                key_id,
                secret_sealed,
                name: name.to_owned(),
                replaces: replaces.clone(),
            });
            match self.propose_identity(command).await? {
                Ok(Some(created)) => {
                    return Ok(Exchanged {
                        key: decode(&created, AccessKey::from_resource)?,
                        secret: key_secret,
                        user,
                    });
                }
                Ok(None) => {
                    return Err(GroupError::Unavailable(Detail::new("no access key")).into());
                }
                // Another write changed the code since it was read, or the
                // drawn id was in use: read and draw again.
                Err(Refusal::Resource(
                    ResourceError::Conflict { .. } | ResourceError::AlreadyExists,
                )) => {}
                Err(Refusal::Resource(ResourceError::NotFound)) => {
                    return Err(IdentityError::Unauthenticated);
                }
                Err(refusal) => return Err(refusal.into()),
            }
        }
        Err(
            GroupError::Unavailable(Detail::new("the exchange did not commit in 8 attempts"))
                .into(),
        )
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    const NOW: WallTime = WallTime(1_800_000_000_000_000_000);
    const USER: Uid = Uid([2; 16]);

    fn org(enabled: bool) -> Resource {
        Org {
            name: "default".to_owned(),
            uid: Uid([1; 16]),
            generation: 1,
            labels: BTreeMap::new(),
            deleting: false,
            enabled,
        }
        .to_resource()
    }

    fn user(enabled: bool) -> Resource {
        User {
            org: "default".to_owned(),
            name: "admin".to_owned(),
            uid: USER,
            generation: 1,
            labels: BTreeMap::new(),
            deleting: false,
            enabled,
            cluster_roles: BTreeSet::from([ClusterRole::CloudAdmin]),
        }
        .to_resource()
    }

    fn code(id: u8, expire_time: WallTime, state: CodeState) -> LoginCodeRecord {
        LoginCodeRecord {
            org: "default".to_owned(),
            code_id: CodeId([id; 8]),
            uid: Uid([id; 16]),
            generation: 1,
            labels: BTreeMap::new(),
            deleting: false,
            user: USER,
            secret_hash: [9; 32],
            issued_by: IssuedBy::Init,
            expire_time,
            state,
        }
    }

    fn later(secs: i64) -> WallTime {
        WallTime(NOW.0 + secs * 1_000_000_000)
    }

    fn key(id: u8, name: &str, revoked: bool) -> Resource {
        AccessKey {
            org: "default".to_owned(),
            id: AccessKeyId::from_random(&[id; 10]),
            uid: Uid([id; 16]),
            generation: 1,
            labels: BTreeMap::new(),
            deleting: false,
            principal: USER,
            key_name: name.to_owned(),
            secret_sealed: vec![0; crate::iam::SEALED_SECRET_LEN],
            issued_by: IssuedBy::Init,
            revoked,
        }
        .to_resource()
    }

    fn exchange(name: &str) -> Exchange {
        Exchange {
            code: code(1, later(60), CodeState::Unused).to_resource().target(),
            generation: 1,
            user: "admin".to_owned(),
            now: NOW,
            key_uid: Uid([0xee; 16]),
            key_id: AccessKeyId::from_random(&[0xee; 10]),
            secret_sealed: vec![1; crate::iam::SEALED_SECRET_LEN],
            name: name.to_owned(),
            replaces: None,
        }
    }

    type Writes = Vec<(Vec<u8>, Option<Resource>)>;

    fn run(
        x: &Exchange,
        code: &LoginCodeRecord,
        keys: &[Resource],
    ) -> Result<(Writes, AccessKey), Refusal> {
        let (writes, created) = decide_exchange(
            x,
            Some(code.to_resource()),
            Some(user(true)),
            Some(org(true)),
            keys,
            false,
        )?;
        Ok((writes, AccessKey::from_resource(&created).unwrap()))
    }

    #[test]
    fn an_exchange_spends_the_code_and_creates_one_key() {
        let unused = code(1, later(60), CodeState::Unused);
        let (writes, created) = run(&exchange("laptop"), &unused, &[]).unwrap();
        assert_eq!(writes.len(), 2);
        let spent = LoginCodeRecord::from_resource(writes[0].1.as_ref().unwrap()).unwrap();
        assert_eq!(spent.state, CodeState::Spent);
        assert_eq!(spent.generation, 2);
        assert_eq!(created.key_name, "laptop");
        assert_eq!(created.principal, USER);
        assert_eq!(created.issued_by, IssuedBy::Init);
        assert!(!created.revoked);
    }

    #[test]
    fn a_code_is_refused_when_spent_voided_or_expired() {
        let x = exchange("laptop");
        let cases = [
            (code(1, later(60), CodeState::Spent), Refusal::Used),
            (code(1, later(60), CodeState::Voided), Refusal::Voided),
            (code(1, NOW, CodeState::Unused), Refusal::Expired(NOW)),
            (
                code(1, later(-1), CodeState::Unused),
                Refusal::Expired(later(-1)),
            ),
        ];
        for (code, refusal) in cases {
            assert_eq!(run(&x, &code, &[]).map(drop), Err(refusal));
        }
        let mut moved = x.clone();
        moved.generation = 2;
        let unused = code(1, later(60), CodeState::Unused);
        assert!(matches!(
            run(&moved, &unused, &[]),
            Err(Refusal::Resource(ResourceError::Conflict { .. }))
        ));
    }

    #[test]
    fn the_checks_at_commit_come_in_the_order_of_section_3_7() {
        // A used code is refused before a bad name and a disabled user.
        let spent = code(1, later(60), CodeState::Spent);
        let bad = exchange("Bad");
        let refused = decide_exchange(
            &bad,
            Some(spent.to_resource()),
            Some(user(false)),
            Some(org(true)),
            &[],
            false,
        );
        assert_eq!(refused.map(drop), Err(Refusal::Used));
        let unused = code(1, later(60), CodeState::Unused);
        let refused = decide_exchange(
            &bad,
            Some(unused.to_resource()),
            Some(user(false)),
            Some(org(true)),
            &[],
            false,
        );
        assert!(matches!(refused, Err(Refusal::InvalidName(_))));
        for (u, o) in [(user(false), org(true)), (user(true), org(false))] {
            let refused = decide_exchange(
                &exchange("laptop"),
                Some(unused.to_resource()),
                Some(u),
                Some(o),
                &[],
                false,
            );
            assert_eq!(refused.map(drop), Err(Refusal::PermissionDenied));
        }
        let gone = decide_exchange(
            &exchange("laptop"),
            Some(unused.to_resource()),
            None,
            Some(org(true)),
            &[],
            false,
        );
        assert_eq!(gone.map(drop), Err(Refusal::PermissionDenied));
        let taken = decide_exchange(
            &exchange("laptop"),
            Some(unused.to_resource()),
            Some(user(true)),
            Some(org(true)),
            &[],
            true,
        );
        assert_eq!(
            taken.map(drop),
            Err(Refusal::Resource(ResourceError::AlreadyExists))
        );
    }

    #[test]
    fn names_in_use_get_the_lowest_free_number() {
        let unused = code(1, later(60), CodeState::Unused);
        let keys = [
            key(10, "laptop", false),
            key(11, "laptop-2", false),
            key(12, "laptop-3", true),
        ];
        let (_, created) = run(&exchange("laptop"), &unused, &keys).unwrap();
        assert_eq!(created.key_name, "laptop-3");
        let long = format!("a{}", "b".repeat(62));
        let keys = [key(10, &long, false)];
        let (_, created) = run(&exchange(&long), &unused, &keys).unwrap();
        assert_eq!(created.key_name, format!("{}-2", &long[..59]));
        assert_eq!(created.key_name.len(), 61);
    }

    #[test]
    fn replaces_revokes_an_unrevoked_key_of_the_user_and_nothing_else() {
        let unused = code(1, later(60), CodeState::Unused);
        let old = key(10, "laptop", false);
        let old_id = AccessKey::from_resource(&old).unwrap().id;
        let mut x = exchange("laptop");
        x.replaces = Some(old_id);
        let (writes, created) = run(&x, &unused, std::slice::from_ref(&old)).unwrap();
        assert_eq!(created.key_name, "laptop");
        assert_eq!(
            writes[0],
            (crate::resource::key(&old.target()).unwrap(), None)
        );
        // A revoked key, or another principal's, is ignored.
        let revoked = key(10, "laptop", true);
        let (writes, _) = run(&x, &unused, std::slice::from_ref(&revoked)).unwrap();
        assert_eq!(writes.len(), 2);
        let mut other = AccessKey::from_resource(&key(10, "laptop", false)).unwrap();
        other.principal = Uid([3; 16]);
        let (writes, created) = run(&x, &unused, &[other.to_resource()]).unwrap();
        assert_eq!(writes.len(), 2);
        assert_eq!(created.key_name, "laptop");
    }

    #[test]
    fn the_key_limits_of_section_7_refuse_and_count_after_replaces() {
        let unused = code(1, later(60), CodeState::Unused);
        let full: Vec<Resource> = (0..MAX_UNREVOKED_KEYS)
            .map(|i| {
                let mut k = AccessKey::from_resource(&key(0, "k", false)).unwrap();
                k.id = AccessKeyId::from_random(&[u8::try_from(i).unwrap(); 10]);
                k.key_name = format!("k{i}");
                k.to_resource()
            })
            .collect();
        assert_eq!(
            run(&exchange("laptop"), &unused, &full).map(drop),
            Err(Refusal::Limit(KeyLimit::Unrevoked))
        );
        let mut x = exchange("laptop");
        x.replaces = Some(AccessKey::from_resource(&full[0]).unwrap().id);
        assert!(run(&x, &unused, &full).is_ok());
        let kept: Vec<Resource> = (0..MAX_KEPT_KEYS)
            .map(|i| {
                let mut k = AccessKey::from_resource(&key(0, "k", true)).unwrap();
                let mut id = [0u8; 10];
                id[..8].copy_from_slice(&(i as u64).to_be_bytes());
                k.id = AccessKeyId::from_random(&id);
                k.to_resource()
            })
            .collect();
        assert_eq!(
            run(&exchange("laptop"), &unused, &kept).map(drop),
            Err(Refusal::Limit(KeyLimit::Kept))
        );
    }

    #[test]
    fn an_issue_deletes_the_oldest_of_ten_unused_codes() {
        let mut held: Vec<Resource> = (0..10u8)
            .map(|i| code(10 + i, later(100 + i64::from(i)), CodeState::Unused).to_resource())
            .collect();
        // Spent and expired codes do not count.
        held.push(code(30, later(5), CodeState::Spent).to_resource());
        held.push(code(31, NOW, CodeState::Unused).to_resource());
        let issue = Issue {
            code: code(40, later(86_400), CodeState::Unused).to_resource(),
            user: "admin".to_owned(),
            now: NOW,
        };
        let (writes, _) =
            decide_issue(&issue, Some(user(true)), Some(org(true)), &held, false).unwrap();
        assert_eq!(writes.len(), 2);
        assert_eq!(
            writes[0],
            (crate::resource::key(&held[0].target()).unwrap(), None)
        );
        let (writes, _) =
            decide_issue(&issue, Some(user(true)), Some(org(true)), &held[1..], false).unwrap();
        assert_eq!(writes.len(), 1);
        assert_eq!(
            decide_issue(&issue, Some(user(false)), Some(org(true)), &[], false).map(drop),
            Err(Refusal::PermissionDenied)
        );
        assert_eq!(
            decide_issue(&issue, Some(user(true)), None, &[], false).map(drop),
            Err(Refusal::PermissionDenied)
        );
        assert_eq!(
            decide_issue(&issue, None, Some(org(true)), &[], false).map(drop),
            Err(Refusal::Resource(ResourceError::NotFound))
        );
        assert_eq!(
            decide_issue(&issue, Some(user(true)), Some(org(true)), &[], true).map(drop),
            Err(Refusal::Resource(ResourceError::AlreadyExists))
        );
    }

    #[test]
    fn identity_commands_round_trip() {
        let mut x = exchange("laptop");
        x.replaces = Some(AccessKeyId::from_random(&[4; 10]));
        let issue = Issue {
            code: code(40, later(86_400), CodeState::Unused).to_resource(),
            user: "admin".to_owned(),
            now: WallTime(-5),
        };
        for command in [
            IdentityCommand::Exchange(x.clone()),
            IdentityCommand::Exchange(exchange("a")),
            IdentityCommand::Issue(issue),
        ] {
            let mut enc = Enc::default();
            command.encode(&mut enc);
            let mut dec = Dec(&enc.0);
            assert_eq!(IdentityCommand::decode(&mut dec), Some(command));
            assert_eq!(dec.end(), Some(()));
        }
        assert!(!format!("{x:?}").contains("[1, 1"));
    }

    #[test]
    fn hashes_compare_equal_only_when_equal() {
        let a = [7u8; 32];
        let mut b = a;
        assert!(same_hash(&a, &b));
        b[31] ^= 1;
        assert!(!same_hash(&a, &b));
    }
}
