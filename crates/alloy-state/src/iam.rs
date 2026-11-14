//! The identity kinds of the global group (ARFC 0007 section 2.5) that M11
//! rung 2 builds (ARFC 0007 section 3.11, ARFC 0002 section 3.15): `Org`,
//! `User`, `AccessKey`, and `LoginCode`, in service `iam`.
//!
//! Each kind's `spec` and `status` are this module's Protocol Buffers
//! messages. No method serves these kinds yet, so their field numbers are
//! this crate's choice (ARFC 0008 clause 3.1). No field holds a secret in
//! clear: a login code keeps `BLAKE3` of its secret, and an access key its
//! secret sealed under the at-rest key (ARFC 0007 section 3.11).

use std::collections::{BTreeMap, BTreeSet};
use std::fmt;

use alloy_crypto::Zeroizing;
use alloy_rt::WallTime;
use prost::Message as _;

use crate::node::{check_name as check_node_name, timestamp, wall_time};
use crate::resource::{Resource, ResourceError, Scope, Uid};
use crate::token::hex;

/// The kind of org resources.
pub const ORG_KIND: &str = "Org";
/// The kind of user resources.
pub const USER_KIND: &str = "User";
/// The kind of access key resources.
pub const ACCESS_KEY_KIND: &str = "AccessKey";
/// The kind of login code resources.
pub const LOGIN_CODE_KIND: &str = "LoginCode";

/// The longest name of an org, user, or access key (ARFC 0007 section 7).
pub const MAX_NAME: usize = 63;
/// A key name is cut to this many characters before a number is appended
/// (ARFC 0007 section 7).
pub const KEY_NAME_CUT: usize = 59;

/// The most unrevoked access keys of one principal (ARFC 0007 section 7).
pub const MAX_UNREVOKED_KEYS: usize = 100;
/// The most kept access keys of one user (ARFC 0007 section 7).
pub const MAX_KEPT_KEYS: usize = 1_000;
/// The most unused, unexpired login codes of one user (ARFC 0002 section
/// 3.7).
pub const MAX_UNUSED_CODES: usize = 10;
/// How long a login code is valid after it is issued (ARFC 0002 section
/// 3.7), in nanoseconds of wall time.
pub const CODE_LIFETIME_NANOS: i64 = 24 * 3_600 * 1_000_000_000;

/// The length of `secret_sealed`: a 24-byte nonce, then the 40 sealed bytes
/// of the secret and the 16-byte tag (ARFC 0007 section 3.11).
pub const SEALED_SECRET_LEN: usize = 24 + 40 + 16;

/// Checks a name of ARFC 0007 section 7: 1 to 63 characters of `a` to `z`,
/// `0` to `9`, and `-`, starting with a letter.
///
/// # Errors
///
/// [`ResourceError::Invalid`] naming the rule.
pub fn check_name(name: &str) -> Result<(), ResourceError> {
    let ok = name.len() <= MAX_NAME
        && name.bytes().next().is_some_and(|b| b.is_ascii_lowercase())
        && name
            .bytes()
            .all(|b| b.is_ascii_lowercase() || b.is_ascii_digit() || b == b'-');
    if ok {
        Ok(())
    } else {
        Err(ResourceError::Invalid(
            "a name must be 1 to 63 characters of a-z, 0-9, and -, starting with a letter",
        ))
    }
}

/// A cluster role a user holds (ARFC 0007 section 2.4).
#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub enum ClusterRole {
    /// `cloud-admin`.
    CloudAdmin,
    /// `cloud-viewer`.
    CloudViewer,
}

impl ClusterRole {
    /// The role as ARFC 0007 spells it.
    #[must_use]
    pub fn as_str(self) -> &'static str {
        match self {
            ClusterRole::CloudAdmin => "cloud-admin",
            ClusterRole::CloudViewer => "cloud-viewer",
        }
    }

    /// The role spelled `text`, or `None`.
    #[must_use]
    pub fn parse(text: &str) -> Option<ClusterRole> {
        match text {
            "cloud-admin" => Some(ClusterRole::CloudAdmin),
            "cloud-viewer" => Some(ClusterRole::CloudViewer),
            _ => None,
        }
    }
}

/// What issued a login code, and so what a user access key made from it
/// records as `issued_by` (ARFC 0007 clause 3.9.5). M11 rung 2 issues login
/// codes only through `init` and a local admin (ARFC 0007 section 3.11), so
/// these are the only issuers this crate stores; no key descends from
/// another key.
#[derive(Clone, Debug, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub enum IssuedBy {
    /// `init`.
    Init,
    /// `root@<node name>`: the local admin of that control node.
    LocalAdmin(String),
}

impl IssuedBy {
    /// The issuer as ARFC 0007 clause 3.9.5 spells it.
    #[must_use]
    pub fn to_text(&self) -> String {
        match self {
            IssuedBy::Init => "init".to_owned(),
            IssuedBy::LocalAdmin(node) => format!("root@{node}"),
        }
    }

    /// The issuer spelled `text`, or `None`: `init`, or `root@` and a node
    /// name.
    #[must_use]
    pub fn parse(text: &str) -> Option<IssuedBy> {
        if text == "init" {
            return Some(IssuedBy::Init);
        }
        let node = text.strip_prefix("root@")?;
        check_node_name(node).ok()?;
        Some(IssuedBy::LocalAdmin(node.to_owned()))
    }
}

/// A login code's 8-byte public lookup id (ARFC 0002 section 3.7).
#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub struct CodeId(pub [u8; 8]);

impl CodeId {
    /// The name of the code's resource: `code-` and 16 lower case
    /// hexadecimal digits.
    #[must_use]
    pub fn resource_name(&self) -> String {
        format!("code-{}", hex(&self.0))
    }
}

/// An access key id: `AKAL` followed by 16 characters of the RFC 4648
/// section 6 base32 alphabet in upper case (ARFC 0007 section 2.3).
#[derive(Clone, Debug, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub struct AccessKeyId(String);

impl AccessKeyId {
    /// The id of the 10 random bytes `bytes`: `AKAL` and their unpadded
    /// base32 encoding.
    #[must_use]
    pub fn from_random(bytes: &[u8; 10]) -> AccessKeyId {
        AccessKeyId(format!("AKAL{}", base32_upper(bytes)))
    }

    /// The id spelled `text`, or `None` when it is not spelled as one.
    #[must_use]
    pub fn parse(text: &str) -> Option<AccessKeyId> {
        let rest = text.strip_prefix("AKAL")?;
        // 16 characters hold 80 bits exactly, so every character of the
        // alphabet is valid in every place and each id is one encoding.
        let ok = rest.len() == 16
            && rest
                .bytes()
                .all(|b| b.is_ascii_uppercase() || (b'2'..=b'7').contains(&b));
        ok.then(|| AccessKeyId(text.to_owned()))
    }

    /// The id as spelled, 20 ASCII characters.
    #[must_use]
    pub fn as_str(&self) -> &str {
        &self.0
    }

    /// The name of the key's resource: the id in lower case.
    #[must_use]
    pub fn resource_name(&self) -> String {
        self.0.to_ascii_lowercase()
    }

    /// The id of the resource named `name`, or `None`.
    fn from_resource_name(name: &str) -> Option<AccessKeyId> {
        let upper = name.to_ascii_uppercase();
        (upper.to_ascii_lowercase() == name)
            .then(|| AccessKeyId::parse(&upper))
            .flatten()
    }
}

/// An access key's secret: 40 characters of the RFC 4648 section 5
/// base64url alphabet (ARFC 0007 section 2.3). It is erased when dropped
/// and shows nothing through `Debug`.
pub struct AccessKeySecret(Zeroizing<String>);

impl AccessKeySecret {
    /// The secret of the 30 random bytes `bytes`: their unpadded base64url
    /// encoding.
    #[must_use]
    pub fn from_random(bytes: &[u8; 30]) -> AccessKeySecret {
        AccessKeySecret(Zeroizing::new(base64url(bytes)))
    }

    /// The secret's 40 ASCII characters. The caller must not log them.
    #[must_use]
    pub fn expose(&self) -> &str {
        &self.0
    }
}

/// Shows only the type name.
impl fmt::Debug for AccessKeySecret {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("AccessKeySecret").finish_non_exhaustive()
    }
}

/// Unpadded RFC 4648 section 6 base32, upper case.
pub(crate) fn base32_upper(bytes: &[u8]) -> String {
    const ALPHABET: &[u8; 32] = b"ABCDEFGHIJKLMNOPQRSTUVWXYZ234567";
    let mut out = String::with_capacity(bytes.len().div_ceil(5) * 8);
    let mut buffer: u32 = 0;
    let mut bits = 0;
    for &byte in bytes {
        buffer = (buffer << 8) | u32::from(byte);
        bits += 8;
        while bits >= 5 {
            bits -= 5;
            out.push(char::from(ALPHABET[((buffer >> bits) & 31) as usize]));
        }
    }
    if bits > 0 {
        out.push(char::from(ALPHABET[((buffer << (5 - bits)) & 31) as usize]));
    }
    out
}

/// Unpadded RFC 4648 section 5 base64url.
pub(crate) fn base64url(bytes: &[u8]) -> String {
    const ALPHABET: &[u8; 64] = b"ABCDEFGHIJKLMNOPQRSTUVWXYZabcdefghijklmnopqrstuvwxyz0123456789-_";
    let mut out = String::with_capacity(bytes.len().div_ceil(3) * 4);
    let mut buffer: u32 = 0;
    let mut bits = 0;
    for &byte in bytes {
        buffer = (buffer << 8) | u32::from(byte);
        bits += 8;
        while bits >= 6 {
            bits -= 6;
            out.push(char::from(ALPHABET[((buffer >> bits) & 63) as usize]));
        }
    }
    if bits > 0 {
        out.push(char::from(ALPHABET[((buffer << (6 - bits)) & 63) as usize]));
    }
    out
}

/// An org (ARFC 0007 section 2.5), with cluster scope.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Org {
    /// Its name, as [`check_name`] allows.
    pub name: String,
    /// Set by the group at creation.
    pub uid: Uid,
    /// As [`Resource::generation`].
    pub generation: u64,
    /// Its labels.
    pub labels: BTreeMap<String, String>,
    /// As [`Resource::deleting`].
    pub deleting: bool,
    /// Whether it is enabled.
    pub enabled: bool,
}

/// A user (ARFC 0007 section 2.5), in the scope of its org.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct User {
    /// The name of its org.
    pub org: String,
    /// Its name, as [`check_name`] allows.
    pub name: String,
    /// Set by the group at creation.
    pub uid: Uid,
    /// As [`Resource::generation`].
    pub generation: u64,
    /// Its labels.
    pub labels: BTreeMap<String, String>,
    /// As [`Resource::deleting`].
    pub deleting: bool,
    /// Whether it is enabled.
    pub enabled: bool,
    /// Its cluster roles.
    pub cluster_roles: BTreeSet<ClusterRole>,
}

/// A user access key (ARFC 0007 section 2.5), in the scope of its user's
/// org.
#[derive(Clone, PartialEq, Eq)]
pub struct AccessKey {
    /// The name of its user's org.
    pub org: String,
    /// Its id; its resource is named by the id in lower case.
    pub id: AccessKeyId,
    /// Set by the group at creation.
    pub uid: Uid,
    /// As [`Resource::generation`].
    pub generation: u64,
    /// Its labels.
    pub labels: BTreeMap<String, String>,
    /// As [`Resource::deleting`].
    pub deleting: bool,
    /// The `uid` of its user.
    pub principal: Uid,
    /// Its name (ARFC 0007 clause 3.9.3), unique among the unrevoked keys
    /// of its user.
    pub key_name: String,
    /// The secret sealed as ARFC 0007 section 3.11 says,
    /// [`SEALED_SECRET_LEN`] bytes.
    pub secret_sealed: Vec<u8>,
    /// What issued the login code it was made from.
    pub issued_by: IssuedBy,
    /// Whether it is revoked.
    pub revoked: bool,
}

/// Shows the length of `secret_sealed` and nothing of its bytes (I-22).
impl fmt::Debug for AccessKey {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("AccessKey")
            .field("org", &self.org)
            .field("id", &self.id)
            .field("uid", &self.uid)
            .field("generation", &self.generation)
            .field("labels", &self.labels)
            .field("deleting", &self.deleting)
            .field("principal", &self.principal)
            .field("key_name", &self.key_name)
            .field("secret_sealed_len", &self.secret_sealed.len())
            .field("issued_by", &self.issued_by)
            .field("revoked", &self.revoked)
            .finish()
    }
}

/// The state of a login code (ARFC 0007 section 2.5).
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub enum CodeState {
    /// Not yet exchanged.
    Unused,
    /// Exchanged, or presented with a wrong secret (ARFC 0002 section 3.7).
    Spent,
    /// Voided (ARFC 0007 clauses 3.9.7 and 3.9.8).
    Voided,
}

/// A login code (ARFC 0007 section 2.5), in the scope of its user's org.
#[derive(Clone, PartialEq, Eq)]
pub struct LoginCodeRecord {
    /// The name of its user's org.
    pub org: String,
    /// Its public lookup id; its resource is named by
    /// [`CodeId::resource_name`].
    pub code_id: CodeId,
    /// Set by the group at creation.
    pub uid: Uid,
    /// As [`Resource::generation`].
    pub generation: u64,
    /// Its labels.
    pub labels: BTreeMap<String, String>,
    /// As [`Resource::deleting`].
    pub deleting: bool,
    /// The `uid` of the user it is for.
    pub user: Uid,
    /// `BLAKE3` of its secret.
    pub secret_hash: [u8; 32],
    /// What issued it.
    pub issued_by: IssuedBy,
    /// When it expires: 24 hours after it was issued.
    pub expire_time: WallTime,
    /// Whether it is unused, spent, or voided.
    pub state: CodeState,
}

/// Shows no byte of `secret_hash`.
impl fmt::Debug for LoginCodeRecord {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("LoginCodeRecord")
            .field("org", &self.org)
            .field("code_id", &hex(&self.code_id.0))
            .field("uid", &self.uid)
            .field("generation", &self.generation)
            .field("labels", &self.labels)
            .field("deleting", &self.deleting)
            .field("user", &self.user)
            .field("issued_by", &self.issued_by)
            .field("expire_time", &self.expire_time)
            .field("state", &self.state)
            .finish_non_exhaustive()
    }
}

#[derive(Clone, PartialEq, prost::Message)]
struct OrgSpecPb {
    #[prost(bool, tag = "1")]
    enabled: bool,
}

#[derive(Clone, PartialEq, prost::Message)]
struct UserSpecPb {
    #[prost(bool, tag = "1")]
    enabled: bool,
    #[prost(string, repeated, tag = "2")]
    cluster_roles: Vec<String>,
}

#[derive(Clone, PartialEq, prost::Message)]
struct AccessKeySpecPb {
    #[prost(bytes = "vec", tag = "1")]
    principal: Vec<u8>,
    #[prost(string, tag = "2")]
    key_name: String,
    #[prost(bytes = "vec", tag = "3")]
    secret_sealed: Vec<u8>,
    #[prost(string, tag = "4")]
    issued_by: String,
}

#[derive(Clone, PartialEq, prost::Message)]
struct AccessKeyStatusPb {
    #[prost(bool, tag = "1")]
    revoked: bool,
}

#[derive(Clone, PartialEq, prost::Message)]
struct LoginCodeSpecPb {
    #[prost(bytes = "vec", tag = "1")]
    code_id: Vec<u8>,
    #[prost(bytes = "vec", tag = "2")]
    user: Vec<u8>,
    #[prost(bytes = "vec", tag = "3")]
    secret_hash: Vec<u8>,
    #[prost(string, tag = "4")]
    issued_by: String,
    #[prost(message, optional, tag = "5")]
    expire_time: Option<prost_types::Timestamp>,
}

#[derive(Clone, PartialEq, prost::Message)]
struct LoginCodeStatusPb {
    /// 0 unused, 1 spent, 2 voided.
    #[prost(uint32, tag = "1")]
    state: u32,
}

fn org_of(scope: &Scope) -> Option<&str> {
    match scope {
        Scope::Org(org) if check_name(org).is_ok() => Some(org),
        _ => None,
    }
}

impl Org {
    /// The resource that stores this org.
    pub(crate) fn to_resource(&self) -> Resource {
        Resource {
            kind: ORG_KIND.to_owned(),
            uid: self.uid,
            name: self.name.clone(),
            scope: Scope::Cluster,
            generation: self.generation,
            spec: OrgSpecPb {
                enabled: self.enabled,
            }
            .encode_to_vec(),
            status: Vec::new(),
            labels: self.labels.clone(),
            deleting: self.deleting,
        }
    }

    /// The org a stored resource holds, or `None` when it is not one.
    pub(crate) fn from_resource(r: &Resource) -> Option<Org> {
        if r.kind != ORG_KIND || r.scope != Scope::Cluster || check_name(&r.name).is_err() {
            return None;
        }
        let spec = OrgSpecPb::decode(r.spec.as_slice()).ok()?;
        r.status.is_empty().then_some(())?;
        Some(Org {
            name: r.name.clone(),
            uid: r.uid,
            generation: r.generation,
            labels: r.labels.clone(),
            deleting: r.deleting,
            enabled: spec.enabled,
        })
    }
}

impl User {
    /// The resource that stores this user.
    pub(crate) fn to_resource(&self) -> Resource {
        Resource {
            kind: USER_KIND.to_owned(),
            uid: self.uid,
            name: self.name.clone(),
            scope: Scope::Org(self.org.clone()),
            generation: self.generation,
            spec: UserSpecPb {
                enabled: self.enabled,
                cluster_roles: self
                    .cluster_roles
                    .iter()
                    .map(|r| r.as_str().to_owned())
                    .collect(),
            }
            .encode_to_vec(),
            status: Vec::new(),
            labels: self.labels.clone(),
            deleting: self.deleting,
        }
    }

    /// The user a stored resource holds, or `None` when it is not one.
    pub(crate) fn from_resource(r: &Resource) -> Option<User> {
        let org = org_of(&r.scope)?;
        if r.kind != USER_KIND || check_name(&r.name).is_err() {
            return None;
        }
        let spec = UserSpecPb::decode(r.spec.as_slice()).ok()?;
        r.status.is_empty().then_some(())?;
        let mut cluster_roles = BTreeSet::new();
        for text in &spec.cluster_roles {
            let role = ClusterRole::parse(text)?;
            // Ascending with no repeats, as written.
            if cluster_roles.last().is_some_and(|last| *last >= role) {
                return None;
            }
            cluster_roles.insert(role);
        }
        Some(User {
            org: org.to_owned(),
            name: r.name.clone(),
            uid: r.uid,
            generation: r.generation,
            labels: r.labels.clone(),
            deleting: r.deleting,
            enabled: spec.enabled,
            cluster_roles,
        })
    }
}

impl AccessKey {
    /// The resource that stores this key.
    pub(crate) fn to_resource(&self) -> Resource {
        Resource {
            kind: ACCESS_KEY_KIND.to_owned(),
            uid: self.uid,
            name: self.id.resource_name(),
            scope: Scope::Org(self.org.clone()),
            generation: self.generation,
            spec: AccessKeySpecPb {
                principal: self.principal.0.to_vec(),
                key_name: self.key_name.clone(),
                secret_sealed: self.secret_sealed.clone(),
                issued_by: self.issued_by.to_text(),
            }
            .encode_to_vec(),
            status: AccessKeyStatusPb {
                revoked: self.revoked,
            }
            .encode_to_vec(),
            labels: self.labels.clone(),
            deleting: self.deleting,
        }
    }

    /// The key a stored resource holds, or `None` when it is not one.
    pub(crate) fn from_resource(r: &Resource) -> Option<AccessKey> {
        let org = org_of(&r.scope)?;
        if r.kind != ACCESS_KEY_KIND {
            return None;
        }
        let id = AccessKeyId::from_resource_name(&r.name)?;
        let spec = AccessKeySpecPb::decode(r.spec.as_slice()).ok()?;
        let status = AccessKeyStatusPb::decode(r.status.as_slice()).ok()?;
        check_name(&spec.key_name).ok()?;
        (spec.secret_sealed.len() == SEALED_SECRET_LEN).then_some(())?;
        Some(AccessKey {
            org: org.to_owned(),
            id,
            uid: r.uid,
            generation: r.generation,
            labels: r.labels.clone(),
            deleting: r.deleting,
            principal: Uid(spec.principal.try_into().ok()?),
            key_name: spec.key_name,
            secret_sealed: spec.secret_sealed,
            issued_by: IssuedBy::parse(&spec.issued_by)?,
            revoked: status.revoked,
        })
    }
}

impl LoginCodeRecord {
    /// The resource that stores this code.
    pub(crate) fn to_resource(&self) -> Resource {
        let state = match self.state {
            CodeState::Unused => 0,
            CodeState::Spent => 1,
            CodeState::Voided => 2,
        };
        Resource {
            kind: LOGIN_CODE_KIND.to_owned(),
            uid: self.uid,
            name: self.code_id.resource_name(),
            scope: Scope::Org(self.org.clone()),
            generation: self.generation,
            spec: LoginCodeSpecPb {
                code_id: self.code_id.0.to_vec(),
                user: self.user.0.to_vec(),
                secret_hash: self.secret_hash.to_vec(),
                issued_by: self.issued_by.to_text(),
                expire_time: Some(timestamp(self.expire_time)),
            }
            .encode_to_vec(),
            status: LoginCodeStatusPb { state }.encode_to_vec(),
            labels: self.labels.clone(),
            deleting: self.deleting,
        }
    }

    /// The code a stored resource holds, or `None` when it is not one.
    pub(crate) fn from_resource(r: &Resource) -> Option<LoginCodeRecord> {
        let org = org_of(&r.scope)?;
        if r.kind != LOGIN_CODE_KIND {
            return None;
        }
        let spec = LoginCodeSpecPb::decode(r.spec.as_slice()).ok()?;
        let status = LoginCodeStatusPb::decode(r.status.as_slice()).ok()?;
        let code_id = CodeId(spec.code_id.try_into().ok()?);
        if r.name != code_id.resource_name() {
            return None;
        }
        let state = match status.state {
            0 => CodeState::Unused,
            1 => CodeState::Spent,
            2 => CodeState::Voided,
            _ => return None,
        };
        Some(LoginCodeRecord {
            org: org.to_owned(),
            code_id,
            uid: r.uid,
            generation: r.generation,
            labels: r.labels.clone(),
            deleting: r.deleting,
            user: Uid(spec.user.try_into().ok()?),
            secret_hash: spec.secret_hash.try_into().ok()?,
            issued_by: IssuedBy::parse(&spec.issued_by)?,
            expire_time: wall_time(spec.expire_time.as_ref()?)?,
            state,
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn base32_and_base64url_match_rfc_4648_section_10() {
        let cases = [
            ("", "", ""),
            ("f", "MY", "Zg"),
            ("fo", "MZXQ", "Zm8"),
            ("foo", "MZXW6", "Zm9v"),
            ("foob", "MZXW6YQ", "Zm9vYg"),
            ("fooba", "MZXW6YTB", "Zm9vYmE"),
            ("foobar", "MZXW6YTBOI", "Zm9vYmFy"),
        ];
        for (input, b32, b64) in cases {
            assert_eq!(base32_upper(input.as_bytes()), b32, "{input}");
            assert_eq!(base64url(input.as_bytes()), b64, "{input}");
        }
        // The two characters where base64url differs from base64.
        assert_eq!(base64url(&[0xfb, 0xff, 0xbf]), "-_-_");
    }

    #[test]
    fn ids_and_secrets_have_the_lengths_of_section_2_3() {
        let id = AccessKeyId::from_random(&[0xff; 10]);
        assert_eq!(id.as_str(), "AKAL7777777777777777");
        assert_eq!(id.as_str().len(), 20);
        let id = AccessKeyId::from_random(&[0; 10]);
        assert_eq!(id.as_str(), "AKALAAAAAAAAAAAAAAAA");
        assert_eq!(AccessKeyId::parse(id.as_str()), Some(id.clone()));
        assert_eq!(id.resource_name(), "akalaaaaaaaaaaaaaaaa");
        assert_eq!(
            AccessKeyId::from_resource_name("akalaaaaaaaaaaaaaaaa"),
            Some(id)
        );
        for bad in [
            "AKALAAAAAAAAAAAAAAA",
            "AKALAAAAAAAAAAAAAAAAA",
            "AKALAAAAAAAAAAAAAAA1",
            "akalaaaaaaaaaaaaaaaa",
            "AKIAAAAAAAAAAAAAAAAA",
        ] {
            assert_eq!(AccessKeyId::parse(bad), None, "{bad}");
        }
        assert_eq!(
            AccessKeyId::from_resource_name("AKALAAAAAAAAAAAAAAAA"),
            None
        );
        let secret = AccessKeySecret::from_random(&[0xa5; 30]);
        assert_eq!(secret.expose().len(), 40);
        assert_eq!(format!("{secret:?}"), "AccessKeySecret { .. }");
    }

    #[test]
    fn names_follow_section_7() {
        assert!(check_name("admin").is_ok());
        assert!(check_name("a").is_ok());
        assert!(check_name(&format!("a{}", "-".repeat(62))).is_ok());
        for bad in ["", "1abc", "-a", "Admin", "a_b", "a.b", "a b"] {
            assert!(check_name(bad).is_err(), "{bad:?}");
        }
        assert!(check_name(&"a".repeat(64)).is_err());
    }

    #[test]
    fn issuers_are_spelled_as_clause_3_9_5_says() {
        assert_eq!(IssuedBy::parse("init"), Some(IssuedBy::Init));
        let root = IssuedBy::LocalAdmin("linux-1".to_owned());
        assert_eq!(root.to_text(), "root@linux-1");
        assert_eq!(IssuedBy::parse("root@linux-1"), Some(root));
        for bad in [
            "",
            "Init",
            "root@",
            "root@Linux",
            "session",
            "AKALAAAAAAAAAAAAAAAA",
        ] {
            assert_eq!(IssuedBy::parse(bad), None, "{bad}");
        }
    }

    fn key() -> AccessKey {
        AccessKey {
            org: "default".to_owned(),
            id: AccessKeyId::from_random(&[3; 10]),
            uid: Uid([1; 16]),
            generation: 1,
            labels: BTreeMap::new(),
            deleting: false,
            principal: Uid([2; 16]),
            key_name: "laptop".to_owned(),
            secret_sealed: vec![0x5a; SEALED_SECRET_LEN],
            issued_by: IssuedBy::Init,
            revoked: false,
        }
    }

    fn code() -> LoginCodeRecord {
        LoginCodeRecord {
            org: "default".to_owned(),
            code_id: CodeId([0xfe; 8]),
            uid: Uid([1; 16]),
            generation: 3,
            labels: BTreeMap::new(),
            deleting: false,
            user: Uid([2; 16]),
            secret_hash: [0x77; 32],
            issued_by: IssuedBy::LocalAdmin("ctl".to_owned()),
            expire_time: WallTime(1_700_000_000_000_000_000),
            state: CodeState::Spent,
        }
    }

    #[test]
    fn each_kind_round_trips_through_its_resource() {
        let org = Org {
            name: "default".to_owned(),
            uid: Uid([1; 16]),
            generation: 1,
            labels: BTreeMap::new(),
            deleting: false,
            enabled: true,
        };
        assert_eq!(Org::from_resource(&org.to_resource()), Some(org));
        let user = User {
            org: "default".to_owned(),
            name: "admin".to_owned(),
            uid: Uid([2; 16]),
            generation: 1,
            labels: BTreeMap::new(),
            deleting: false,
            enabled: true,
            cluster_roles: BTreeSet::from([ClusterRole::CloudViewer, ClusterRole::CloudAdmin]),
        };
        assert_eq!(User::from_resource(&user.to_resource()), Some(user));
        let key = key();
        assert_eq!(key.to_resource().name, key.id.resource_name());
        assert_eq!(AccessKey::from_resource(&key.to_resource()), Some(key));
        let code = code();
        assert_eq!(code.to_resource().name, "code-fefefefefefefefe");
        assert_eq!(
            LoginCodeRecord::from_resource(&code.to_resource()),
            Some(code)
        );
    }

    #[test]
    fn stored_kinds_out_of_range_do_not_decode() {
        let mut r = key().to_resource();
        r.scope = Scope::Cluster;
        assert_eq!(AccessKey::from_resource(&r), None);
        let mut short = key();
        short.secret_sealed.pop();
        assert_eq!(AccessKey::from_resource(&short.to_resource()), None);
        let mut misnamed = key();
        misnamed.key_name = "Laptop".to_owned();
        assert_eq!(AccessKey::from_resource(&misnamed.to_resource()), None);
        let mut r = code().to_resource();
        r.name = "code-0000000000000000".to_owned();
        assert_eq!(LoginCodeRecord::from_resource(&r), None);
        let mut r = code().to_resource();
        r.status = LoginCodeStatusPb { state: 3 }.encode_to_vec();
        assert_eq!(LoginCodeRecord::from_resource(&r), None);
        let mut r = code().to_resource();
        r.scope = Scope::Org("Bad".to_owned());
        assert_eq!(LoginCodeRecord::from_resource(&r), None);
        let roles = UserSpecPb {
            enabled: true,
            cluster_roles: vec!["cloud-admin".to_owned(), "cloud-admin".to_owned()],
        };
        let mut user = User {
            org: "default".to_owned(),
            name: "admin".to_owned(),
            uid: Uid([2; 16]),
            generation: 1,
            labels: BTreeMap::new(),
            deleting: false,
            enabled: true,
            cluster_roles: BTreeSet::new(),
        }
        .to_resource();
        user.spec = roles.encode_to_vec();
        assert_eq!(User::from_resource(&user), None);
        let debug = format!("{:?}", key());
        assert!(debug.contains("secret_sealed_len: 80"), "{debug}");
        assert!(!format!("{:?}", code()).contains("119"));
    }
}
