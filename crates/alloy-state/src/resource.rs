//! Resources (ARFC 0006 section 2.1) and the commands a group applies to them,
//! with the generation check of clause 3.5 and the two-phase deletion of
//! clause 3.6.
//!
//! A resource is stored under `r`, its kind, its scope, and its name, each
//! ended by a zero byte, so that one kind's resources are one key range. The
//! value is the resource in this crate's own encoding: `state.redb` contents
//! are not a stable format (ARFC 0006 section 4.2).

use std::collections::BTreeMap;
use std::fmt;

/// The largest resource, in bytes of this crate's encoding (ARFC 0006
/// section 7). A larger one is refused with [`ResourceError::Invalid`] before
/// it is proposed.
pub const MAX_RESOURCE: usize = 1_048_576;

/// A resource's `uid`: 128 random bits.
#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub struct Uid(pub [u8; 16]);

/// Where a resource lives.
#[derive(Clone, Debug, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub enum Scope {
    /// The whole cluster.
    Cluster,
    /// One project of one org.
    Project {
        /// The org's name.
        org: String,
        /// The project's name.
        project: String,
    },
}

/// One resource. `spec` and `status` are the kind's messages, encoded.
#[derive(Clone, PartialEq, Eq)]
pub struct Resource {
    /// The kind, for example `Node`.
    pub kind: String,
    /// Set by the group when the resource is created.
    pub uid: Uid,
    /// Unique within kind and scope.
    pub name: String,
    /// Where the resource lives.
    pub scope: Scope,
    /// 1 at creation, one more after each change.
    pub generation: u64,
    /// What is wanted.
    pub spec: Vec<u8>,
    /// What is.
    pub status: Vec<u8>,
    /// Labels, by key.
    pub labels: BTreeMap<String, String>,
    /// Set by a delete; the resource is removed once what it holds is
    /// released (clause 3.6).
    pub deleting: bool,
}

/// Writes every field but `spec` and `status`, of which it writes lengths
/// only.
impl fmt::Debug for Resource {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("Resource")
            .field("kind", &self.kind)
            .field("uid", &self.uid)
            .field("name", &self.name)
            .field("scope", &self.scope)
            .field("generation", &self.generation)
            .field("spec_len", &self.spec.len())
            .field("status_len", &self.status.len())
            .field("labels", &self.labels)
            .field("deleting", &self.deleting)
            .finish()
    }
}

/// Why a resource operation was refused. A refused write changes nothing.
#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
pub enum ResourceError {
    /// A kind, name, scope, or label is not acceptable.
    #[error("invalid resource: {0}")]
    Invalid(&'static str),
    /// No such resource.
    #[error("the resource does not exist")]
    NotFound,
    /// A resource of that kind, scope, and name exists.
    #[error("the resource already exists")]
    AlreadyExists,
    /// The generation named is not the resource's (clause 3.5).
    #[error("generation {named} is not the current {current}")]
    Conflict {
        /// The generation the write named.
        named: u64,
        /// The resource's generation.
        current: u64,
    },
    /// Removal of a resource that is not being deleted.
    #[error("the resource is not being deleted")]
    NotDeleting,
}

/// The fields a create sets.
#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) struct Target {
    pub(crate) kind: String,
    pub(crate) scope: Scope,
    pub(crate) name: String,
}

/// One write to resources, as a log entry carries it.
#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) enum Command {
    /// Creates `resource` unless its kind, scope, and name are taken.
    Create(Resource),
    /// Replaces spec, status, and labels when `generation` is current.
    Update(Resource),
    /// Marks the resource `deleting` when `generation` is current.
    Delete { target: Target, generation: u64 },
    /// Removes a resource that is `deleting` when `generation` is current.
    Remove { target: Target, generation: u64 },
}

impl fmt::Display for Command {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        let (verb, kind, name) = match self {
            Command::Create(r) => ("create", &r.kind, &r.name),
            Command::Update(r) => ("update", &r.kind, &r.name),
            Command::Delete { target, .. } => ("delete", &target.kind, &target.name),
            Command::Remove { target, .. } => ("remove", &target.kind, &target.name),
        };
        write!(f, "{verb} {kind}/{name}")
    }
}

fn check_part(part: &str, what: &'static str) -> Result<(), ResourceError> {
    if part.is_empty() || part.len() > 253 || part.bytes().any(|b| b == 0) {
        return Err(ResourceError::Invalid(what));
    }
    Ok(())
}

/// The key prefix of every resource of `kind`.
pub(crate) fn kind_prefix(kind: &str) -> Result<Vec<u8>, ResourceError> {
    check_part(kind, "a kind must be 1 to 253 bytes with no zero byte")?;
    let mut key = b"r\0".to_vec();
    key.extend_from_slice(kind.as_bytes());
    key.push(0);
    Ok(key)
}

/// The key of one resource.
pub(crate) fn key(target: &Target) -> Result<Vec<u8>, ResourceError> {
    let mut key = kind_prefix(&target.kind)?;
    match &target.scope {
        Scope::Cluster => key.push(b'c'),
        Scope::Project { org, project } => {
            check_part(org, "an org must be 1 to 253 bytes with no zero byte")?;
            check_part(
                project,
                "a project must be 1 to 253 bytes with no zero byte",
            )?;
            key.push(b'p');
            key.extend_from_slice(org.as_bytes());
            key.push(0);
            key.extend_from_slice(project.as_bytes());
        }
    }
    key.push(0);
    check_part(
        &target.name,
        "a name must be 1 to 253 bytes with no zero byte",
    )?;
    key.extend_from_slice(target.name.as_bytes());
    Ok(key)
}

impl Resource {
    pub(crate) fn target(&self) -> Target {
        Target {
            kind: self.kind.clone(),
            scope: self.scope.clone(),
            name: self.name.clone(),
        }
    }
}

/// A writer of this crate's value encoding: tags, big-endian integers, and
/// length-prefixed bytes.
#[derive(Default)]
pub(crate) struct Enc(pub(crate) Vec<u8>);

impl Enc {
    pub(crate) fn u8(&mut self, value: u8) -> &mut Enc {
        self.0.push(value);
        self
    }

    pub(crate) fn u64(&mut self, value: u64) -> &mut Enc {
        self.0.extend_from_slice(&value.to_be_bytes());
        self
    }

    pub(crate) fn bytes(&mut self, value: &[u8]) -> &mut Enc {
        self.u64(value.len() as u64);
        self.0.extend_from_slice(value);
        self
    }

    fn target(&mut self, target: &Target) -> &mut Enc {
        self.bytes(target.kind.as_bytes());
        match &target.scope {
            Scope::Cluster => self.u8(0),
            Scope::Project { org, project } => {
                self.u8(1).bytes(org.as_bytes()).bytes(project.as_bytes())
            }
        };
        self.bytes(target.name.as_bytes())
    }

    fn resource(&mut self, r: &Resource) -> &mut Enc {
        self.target(&r.target()).bytes(&r.uid.0).u64(r.generation);
        self.bytes(&r.spec)
            .bytes(&r.status)
            .u64(r.labels.len() as u64);
        for (key, value) in &r.labels {
            self.bytes(key.as_bytes()).bytes(value.as_bytes());
        }
        self.u8(u8::from(r.deleting))
    }
}

/// A total reader of [`Enc`]'s encoding: `None` on any short or bad input.
pub(crate) struct Dec<'a>(pub(crate) &'a [u8]);

impl<'a> Dec<'a> {
    pub(crate) fn take(&mut self, len: usize) -> Option<&'a [u8]> {
        let (head, tail) = self.0.split_at_checked(len)?;
        self.0 = tail;
        Some(head)
    }

    pub(crate) fn u8(&mut self) -> Option<u8> {
        Some(self.take(1)?[0])
    }

    pub(crate) fn u64(&mut self) -> Option<u64> {
        Some(u64::from_be_bytes(self.take(8)?.try_into().ok()?))
    }

    pub(crate) fn bytes(&mut self) -> Option<&'a [u8]> {
        let len = usize::try_from(self.u64()?).ok()?;
        self.take(len)
    }

    fn string(&mut self) -> Option<String> {
        String::from_utf8(self.bytes()?.to_vec()).ok()
    }

    fn target(&mut self) -> Option<Target> {
        let kind = self.string()?;
        let scope = match self.u8()? {
            0 => Scope::Cluster,
            1 => Scope::Project {
                org: self.string()?,
                project: self.string()?,
            },
            _ => return None,
        };
        let name = self.string()?;
        Some(Target { kind, scope, name })
    }

    fn resource(&mut self) -> Option<Resource> {
        let Target { kind, scope, name } = self.target()?;
        let uid = Uid(self.bytes()?.try_into().ok()?);
        let generation = self.u64()?;
        let spec = self.bytes()?.to_vec();
        let status = self.bytes()?.to_vec();
        let mut labels = BTreeMap::new();
        for _ in 0..self.u64()? {
            labels.insert(self.string()?, self.string()?);
        }
        let deleting = match self.u8()? {
            0 => false,
            1 => true,
            _ => return None,
        };
        Some(Resource {
            kind,
            uid,
            name,
            scope,
            generation,
            spec,
            status,
            labels,
            deleting,
        })
    }

    pub(crate) fn end(&self) -> Option<()> {
        self.0.is_empty().then_some(())
    }
}

pub(crate) fn encode_resource(resource: &Resource) -> Vec<u8> {
    let mut enc = Enc::default();
    enc.resource(resource);
    enc.0
}

pub(crate) fn decode_resource(bytes: &[u8]) -> Option<Resource> {
    let mut dec = Dec(bytes);
    let resource = dec.resource()?;
    dec.end()?;
    Some(resource)
}

pub(crate) fn encode_command(command: &Command) -> Vec<u8> {
    let mut enc = Enc::default();
    match command {
        Command::Create(r) => enc.u8(0).resource(r),
        Command::Update(r) => enc.u8(1).resource(r),
        Command::Delete { target, generation } => enc.u8(2).target(target).u64(*generation),
        Command::Remove { target, generation } => enc.u8(3).target(target).u64(*generation),
    };
    enc.0
}

pub(crate) fn decode_command(bytes: &[u8]) -> Option<Command> {
    let mut dec = Dec(bytes);
    let command = match dec.u8()? {
        0 => Command::Create(dec.resource()?),
        1 => Command::Update(dec.resource()?),
        2 => Command::Delete {
            target: dec.target()?,
            generation: dec.u64()?,
        },
        3 => Command::Remove {
            target: dec.target()?,
            generation: dec.u64()?,
        },
        _ => return None,
    };
    dec.end()?;
    Some(command)
}

/// What applying a command does: the key it writes, the value it leaves
/// there (`None` removes it), and the answer to the writer.
pub(crate) type Outcome = Result<(Vec<u8>, Option<Resource>), ResourceError>;

fn current(found: Option<Resource>, generation: u64) -> Result<Resource, ResourceError> {
    let found = found.ok_or(ResourceError::NotFound)?;
    if found.generation != generation {
        return Err(ResourceError::Conflict {
            named: generation,
            current: found.generation,
        });
    }
    Ok(found)
}

/// Decides a command against the resource stored under its key, `found`.
/// Deterministic: every member decides the same from the same state.
pub(crate) fn decide(command: Command, found: Option<Resource>) -> Outcome {
    match command {
        Command::Create(resource) => {
            let key = key(&resource.target())?;
            if found.is_some() {
                return Err(ResourceError::AlreadyExists);
            }
            Ok((key, Some(resource)))
        }
        Command::Update(update) => {
            let key = key(&update.target())?;
            let mut resource = current(found, update.generation)?;
            resource.generation += 1;
            resource.spec = update.spec;
            resource.status = update.status;
            resource.labels = update.labels;
            Ok((key, Some(resource)))
        }
        Command::Delete { target, generation } => {
            let key = key(&target)?;
            let mut resource = current(found, generation)?;
            resource.generation += 1;
            resource.deleting = true;
            Ok((key, Some(resource)))
        }
        Command::Remove { target, generation } => {
            let key = key(&target)?;
            let resource = current(found, generation)?;
            if !resource.deleting {
                return Err(ResourceError::NotDeleting);
            }
            Ok((key, None))
        }
    }
}

/// The length of `resource` in this crate's encoding, without encoding it.
fn encoded_len(r: &Resource) -> usize {
    let field = |bytes: usize| bytes.saturating_add(8);
    let scope = match &r.scope {
        Scope::Cluster => 1,
        Scope::Project { org, project } => 1 + field(org.len()) + field(project.len()),
    };
    let labels = r.labels.iter().fold(8usize, |total, (key, value)| {
        total
            .saturating_add(field(key.len()))
            .saturating_add(field(value.len()))
    });
    [
        field(r.kind.len()),
        scope,
        field(r.name.len()),
        field(r.uid.0.len()),
        8,
        field(r.spec.len()),
        field(r.status.len()),
        labels,
        1,
    ]
    .into_iter()
    .fold(0, usize::saturating_add)
}

/// Checks what can be checked of a command before it is proposed: its key's
/// parts, and that the resource of a create or update is within
/// [`MAX_RESOURCE`]. So no command a client sends exceeds a store limit: a
/// log entry holds one command of at most about `MAX_RESOURCE` bytes, far
/// below `MAX_ENTRY_PAYLOAD`.
pub(crate) fn check_command(command: &Command) -> Result<(), ResourceError> {
    command_key(command)?;
    match command {
        Command::Create(r) | Command::Update(r) => check_size(r),
        Command::Delete { .. } | Command::Remove { .. } => Ok(()),
    }
}

/// Refuses a resource over [`MAX_RESOURCE`] encoded.
pub(crate) fn check_size(resource: &Resource) -> Result<(), ResourceError> {
    if encoded_len(resource) > MAX_RESOURCE {
        return Err(ResourceError::Invalid("the resource is over 1 MiB encoded"));
    }
    Ok(())
}

/// The key a command reads before it is decided.
pub(crate) fn command_key(command: &Command) -> Result<Vec<u8>, ResourceError> {
    match command {
        Command::Create(r) | Command::Update(r) => key(&r.target()),
        Command::Delete { target, .. } | Command::Remove { target, .. } => key(target),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn node(name: &str) -> Resource {
        Resource {
            kind: "Node".to_owned(),
            uid: Uid([5; 16]),
            name: name.to_owned(),
            scope: Scope::Cluster,
            generation: 1,
            spec: b"spec".to_vec(),
            status: vec![],
            labels: BTreeMap::from([("zone".to_owned(), "a".to_owned())]),
            deleting: false,
        }
    }

    #[test]
    fn resources_and_commands_round_trip() {
        let mut r = node("n1");
        assert_eq!(decode_resource(&encode_resource(&r)), Some(r.clone()));
        r.scope = Scope::Project {
            org: "o".to_owned(),
            project: "p".to_owned(),
        };
        for command in [
            Command::Create(r.clone()),
            Command::Update(r.clone()),
            Command::Delete {
                target: r.target(),
                generation: 3,
            },
            Command::Remove {
                target: r.target(),
                generation: 4,
            },
        ] {
            assert_eq!(decode_command(&encode_command(&command)), Some(command));
        }
        let mut bytes = encode_resource(&r);
        bytes.push(0);
        assert_eq!(decode_resource(&bytes), None);
        assert_eq!(decode_resource(&bytes[..bytes.len() - 3]), None);
    }

    #[test]
    fn writes_check_the_generation() {
        let stored = node("n1");
        let mut update = stored.clone();
        update.generation = 2;
        assert_eq!(
            decide(Command::Update(update.clone()), Some(stored.clone())),
            Err(ResourceError::Conflict {
                named: 2,
                current: 1
            })
        );
        update.generation = 1;
        let (_, after) = decide(Command::Update(update), Some(stored.clone())).unwrap();
        assert_eq!(after.map(|r| r.generation), Some(2));
        assert_eq!(
            decide(Command::Create(stored.clone()), Some(stored.clone())),
            Err(ResourceError::AlreadyExists)
        );
        let remove = Command::Remove {
            target: stored.target(),
            generation: 1,
        };
        assert_eq!(
            decide(remove, Some(stored.clone())),
            Err(ResourceError::NotDeleting)
        );
    }

    #[test]
    fn the_size_check_measures_the_encoding() {
        let mut r = node("n1");
        assert_eq!(encoded_len(&r), encode_resource(&r).len());
        r.scope = Scope::Project {
            org: "org".to_owned(),
            project: "p".to_owned(),
        };
        r.status = vec![1; 300];
        assert_eq!(encoded_len(&r), encode_resource(&r).len());
        let room = MAX_RESOURCE - encoded_len(&r);
        r.spec.extend(vec![0; room]);
        assert_eq!(check_command(&Command::Create(r.clone())), Ok(()));
        r.spec.push(0);
        let over = Err(ResourceError::Invalid("the resource is over 1 MiB encoded"));
        assert_eq!(check_command(&Command::Update(r.clone())), over);
        let mut bad = node("a\0b");
        bad.spec.clear();
        assert!(check_command(&Command::Create(bad)).is_err());
    }

    #[test]
    fn keys_keep_kinds_apart_and_refuse_zero_bytes() {
        let a = key(&node("x").target()).unwrap();
        assert!(a.starts_with(&kind_prefix("Node").unwrap()));
        assert!(!a.starts_with(&kind_prefix("Nod").unwrap()));
        assert!(key(&node("a\0b").target()).is_err());
        assert!(key(&node("").target()).is_err());
    }
}
