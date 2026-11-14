//! One `openraft` group over one [`LogStore`] and one [`StateStore`] (ARFC
//! 0006 sections 2.7 and 2.8, clauses 3.1, 3.2 for one voter, 3.5, 3.6, 3.10
//! without backups, and 3.12).

use std::collections::{BTreeMap, BTreeSet};
use std::fmt;
use std::io;
use std::ops::{Bound, RangeBounds};
use std::sync::Arc;
use std::time::Duration;

use alloy_rt::Runtime;
use alloy_seam::Detail;
use futures_util::StreamExt;
use openraft::async_runtime::WatchReceiver;
use openraft::errors::{RPCError, ReplicationClosed, StreamingError, Unreachable};
use openraft::impls::leader_id_adv::LeaderId;
use openraft::metrics::Metric;
use openraft::network::{RPCOption, RaftNetworkFactory, RaftNetworkV2};
use openraft::raft::{
    AppendEntriesRequest, AppendResult, SnapshotResponse, VoteRequest, VoteResponse,
};
use openraft::storage::IOFlushed;
use openraft::storage::{EntryResponder, RaftLogStorage, RaftStateMachine};
use openraft::{
    Config, EmptyNode, EntryPayload, OptionalSend, Raft, RaftLogReader, RaftSnapshotBuilder,
    ReadPolicy, SnapshotMeta, SnapshotPolicy, StoredMembership,
};
use tokio::sync::Mutex;

use crate::node::{NODE_KIND, Node, Soft};
use crate::resource::{
    Command, Dec, Enc, Resource, ResourceError, Scope, Target, Uid, check_command, check_size,
    command_key, decide, decode_command, decode_resource, encode_command, encode_resource,
    kind_prefix,
};
use crate::rt::{AlloyRt, RtScope, scoped};
use crate::store::{
    ApplyBatch, Entry, LogId, LogStore, MAX_APPEND_BYTES, MAX_SCAN, NodeId, Op, StateSnapshot,
    StateStore, StoreError, Vote,
};

openraft::declare_raft_types!(
    /// The `openraft` types of a group.
    pub(crate) Cfg:
        D = Command,
        R = Result<Option<Resource>, ResourceError>,
        NodeId = u128,
        Node = EmptyNode,
        AsyncRuntime = AlloyRt,
);

type RaftLogId = openraft::LogId<LeaderId<u64, u128>>;
type RaftVote = openraft::Vote<LeaderId<u64, u128>>;
type RaftEntry = openraft::Entry<LeaderId<u64, u128>, EntryPayload<Command, Membership>>;
type Membership = openraft::Membership<u128, EmptyNode>;
type Stored = StoredMembership<LeaderId<u64, u128>, Membership>;
type Snapshot = openraft::Snapshot<LeaderId<u64, u128>, Membership, Box<dyn StateSnapshot>>;

/// The entries after which a member takes a snapshot (clause 3.10).
const SNAPSHOT_EVERY: u64 = 10_000;
/// How long `open` waits for the log to be applied (clause 3.12.3).
const OPEN_TIMEOUT: Duration = Duration::from_secs(60);
/// How long stopping a group waits for its tasks to end.
const STOP_TIMEOUT: Duration = Duration::from_secs(60);
/// The state store key of the group's membership.
const MEMBERSHIP_KEY: &[u8] = b"\0membership";

fn raft_node(node: NodeId) -> u128 {
    u128::from_be_bytes(node.0)
}

fn alloy_node(node: u128) -> NodeId {
    NodeId(node.to_be_bytes())
}

fn to_raft_id(id: LogId) -> RaftLogId {
    openraft::LogId::new(
        LeaderId {
            term: id.term,
            node_id: raft_node(id.leader),
        },
        id.index,
    )
}

fn to_alloy_id(id: &RaftLogId) -> LogId {
    LogId {
        term: id.leader_id.term,
        leader: alloy_node(id.leader_id.node_id),
        index: id.index,
    }
}

fn io_error(error: impl fmt::Display) -> io::Error {
    io::Error::other(error.to_string())
}

fn encode_membership(enc: &mut Enc, membership: &Membership) {
    let configs = membership.get_joint_config();
    enc.u64(configs.len() as u64);
    for config in configs {
        enc.u64(config.len() as u64);
        for node in config {
            enc.bytes(&node.to_be_bytes());
        }
    }
    let nodes: Vec<u128> = membership.nodes().map(|(id, _)| *id).collect();
    enc.u64(nodes.len() as u64);
    for node in nodes {
        enc.bytes(&node.to_be_bytes());
    }
}

fn decode_node(dec: &mut Dec<'_>) -> Option<u128> {
    Some(u128::from_be_bytes(dec.bytes()?.try_into().ok()?))
}

fn decode_membership(dec: &mut Dec<'_>) -> Option<Membership> {
    let mut configs = Vec::new();
    for _ in 0..dec.u64()? {
        let mut config = BTreeSet::new();
        for _ in 0..dec.u64()? {
            config.insert(decode_node(dec)?);
        }
        configs.push(config);
    }
    let mut nodes = BTreeMap::new();
    for _ in 0..dec.u64()? {
        nodes.insert(decode_node(dec)?, EmptyNode {});
    }
    Membership::new(configs, nodes).ok()
}

fn encode_entry(entry: &RaftEntry) -> Entry {
    let mut enc = Enc::default();
    match &entry.payload {
        EntryPayload::Blank => {
            enc.u8(0);
        }
        EntryPayload::Normal(command) => {
            enc.u8(1);
            enc.0.extend_from_slice(&encode_command(command));
        }
        EntryPayload::Membership(membership) => {
            enc.u8(2);
            encode_membership(&mut enc, membership);
        }
    }
    Entry {
        log_id: to_alloy_id(&entry.log_id),
        payload: enc.0,
    }
}

fn decode_entry(entry: Entry) -> Result<RaftEntry, io::Error> {
    let corrupt = || io_error(format!("log entry {} does not decode", entry.log_id.index));
    let mut dec = Dec(&entry.payload);
    let payload = match dec.u8().ok_or_else(corrupt)? {
        0 => EntryPayload::Blank,
        1 => EntryPayload::Normal(decode_command(dec.0).ok_or_else(corrupt)?),
        2 => {
            let membership = decode_membership(&mut dec).ok_or_else(corrupt)?;
            dec.end().ok_or_else(corrupt)?;
            EntryPayload::Membership(membership)
        }
        _ => return Err(corrupt()),
    };
    Ok(openraft::Entry {
        log_id: to_raft_id(entry.log_id),
        payload,
    })
}

/// Splits `entries` into runs that one store `append` accepts, each within
/// `MAX_APPEND_BYTES`. `openraft` may hand over more at once. A stop between
/// two runs leaves a prefix of the entries, as Raft allows: the entries are
/// reported durable only once every run is.
fn append_runs(entries: Vec<Entry>) -> Vec<Vec<Entry>> {
    let mut runs = Vec::new();
    let mut run = Vec::new();
    let mut bytes = 0usize;
    for entry in entries {
        let len = entry.payload.len();
        if !run.is_empty() && bytes.saturating_add(len) > MAX_APPEND_BYTES {
            runs.push(std::mem::take(&mut run));
            bytes = 0;
        }
        bytes = bytes.saturating_add(len);
        run.push(entry);
    }
    if !run.is_empty() {
        runs.push(run);
    }
    runs
}

/// The log store as `openraft` storage.
#[derive(Clone)]
struct LogAdapter(Arc<dyn LogStore>);

impl RaftLogReader<Cfg> for LogAdapter {
    async fn try_get_log_entries<RB: RangeBounds<u64> + Clone + fmt::Debug + OptionalSend>(
        &mut self,
        range: RB,
    ) -> Result<Vec<RaftEntry>, io::Error> {
        let mut first = match range.start_bound() {
            Bound::Included(i) => *i,
            Bound::Excluded(i) => i.saturating_add(1),
            Bound::Unbounded => 0,
        };
        let last = match range.end_bound() {
            Bound::Included(i) => *i,
            Bound::Excluded(0) => return Ok(Vec::new()),
            Bound::Excluded(i) => i - 1,
            Bound::Unbounded => u64::MAX,
        };
        let mut out = Vec::new();
        // `read` stops at MAX_READ_BYTES, so read on after the last returned.
        while first <= last {
            let entries = self.0.read(first, last).await.map_err(io_error)?;
            let next = entries.last().and_then(|e| e.log_id.index.checked_add(1));
            for entry in entries {
                out.push(decode_entry(entry)?);
            }
            match next {
                Some(next) => first = next,
                None => break,
            }
        }
        Ok(out)
    }

    async fn read_vote(&mut self) -> Result<Option<RaftVote>, io::Error> {
        let vote = self.0.read_vote().await.map_err(io_error)?;
        Ok(vote.map(|v| openraft::Vote {
            leader_id: LeaderId {
                term: v.term,
                node_id: raft_node(v.node),
            },
            committed: v.committed,
        }))
    }
}

impl RaftLogStorage<Cfg> for LogAdapter {
    type LogReader = LogAdapter;

    async fn get_log_state(&mut self) -> Result<openraft::LogState<Cfg>, io::Error> {
        let state = self.0.state().await.map_err(io_error)?;
        Ok(openraft::LogState {
            last_purged_log_id: state.last_purged.map(to_raft_id),
            last_log_id: state.last.map(to_raft_id),
        })
    }

    async fn get_log_reader(&mut self) -> LogAdapter {
        self.clone()
    }

    async fn save_vote(&mut self, vote: &RaftVote) -> Result<(), io::Error> {
        let vote = Vote {
            term: vote.leader_id.term,
            node: alloy_node(vote.leader_id.node_id),
            committed: vote.committed,
        };
        self.0.save_vote(vote).await.map_err(io_error)
    }

    async fn append<I>(&mut self, entries: I, callback: IOFlushed<Cfg>) -> Result<(), io::Error>
    where
        I: IntoIterator<Item = RaftEntry> + OptionalSend,
        I::IntoIter: OptionalSend,
    {
        let entries: Vec<Entry> = entries.into_iter().map(|e| encode_entry(&e)).collect();
        // A store's append is durable when it returns (clause 5.3.2).
        let mut result = Ok(());
        for run in append_runs(entries) {
            result = self.0.append(run).await.map_err(io_error);
            if result.is_err() {
                break;
            }
        }
        callback.io_completed(result.as_ref().map(|_| ()).map_err(io_error));
        result
    }

    async fn truncate_after(&mut self, last: Option<RaftLogId>) -> Result<(), io::Error> {
        let from = last.map_or(0, |id| id.index.saturating_add(1));
        self.0.truncate_from(from).await.map_err(io_error)
    }

    async fn purge(&mut self, log_id: RaftLogId) -> Result<(), io::Error> {
        self.0
            .purge_through(to_alloy_id(&log_id))
            .await
            .map_err(io_error)
    }
}

/// The state store as an `openraft` state machine.
#[derive(Clone)]
struct StateAdapter {
    store: Arc<dyn StateStore>,
    /// Held by each apply, so that a snapshot and its membership are of one
    /// position.
    applying: Arc<Mutex<()>>,
}

impl StateAdapter {
    async fn membership(&self) -> Result<Stored, io::Error> {
        let Some(bytes) = self.store.get(MEMBERSHIP_KEY).await.map_err(io_error)? else {
            return Ok(Stored::default());
        };
        let corrupt = || io_error("the stored membership does not decode");
        let mut dec = Dec(&bytes);
        let log_id = match dec.u8().ok_or_else(corrupt)? {
            0 => None,
            _ => Some(to_raft_id(
                crate::rules::decode_log_id(dec.take(32).ok_or_else(corrupt)?)
                    .ok_or_else(corrupt)?,
            )),
        };
        let membership = decode_membership(&mut dec).ok_or_else(corrupt)?;
        Ok(Stored::new(log_id, membership))
    }

    /// The ops of one entry and the answer to its writer.
    async fn decide(
        &self,
        log_id: &RaftLogId,
        payload: EntryPayload<Command, Membership>,
    ) -> Result<(Vec<Op>, Result<Option<Resource>, ResourceError>), io::Error> {
        match payload {
            EntryPayload::Blank => Ok((Vec::new(), Ok(None))),
            EntryPayload::Membership(membership) => {
                let mut enc = Enc::default();
                enc.u8(1);
                enc.0
                    .extend_from_slice(&crate::rules::encode_log_id(to_alloy_id(log_id)));
                encode_membership(&mut enc, &membership);
                let op = Op::Put {
                    key: MEMBERSHIP_KEY.to_vec(),
                    value: enc.0,
                };
                Ok((vec![op], Ok(None)))
            }
            EntryPayload::Normal(command) => {
                let key = match command_key(&command) {
                    Ok(key) => key,
                    Err(error) => return Ok((Vec::new(), Err(error))),
                };
                let found = match self.store.get(&key).await.map_err(io_error)? {
                    Some(bytes) => Some(
                        decode_resource(&bytes)
                            .ok_or_else(|| io_error("a stored resource does not decode"))?,
                    ),
                    None => None,
                };
                // A remove answers with the resource it removed, so that the
                // leader can drop that node's soft state.
                let removed = match &command {
                    Command::Remove { .. } => found.clone(),
                    _ => None,
                };
                Ok(match decide(command, found) {
                    // `write` refused an oversized resource before proposing
                    // it; this check is deterministic, so every member agrees.
                    Ok((key, Some(resource))) => match check_size(&resource) {
                        Ok(()) => {
                            let value = encode_resource(&resource);
                            (vec![Op::Put { key, value }], Ok(Some(resource)))
                        }
                        Err(error) => (Vec::new(), Err(error)),
                    },
                    Ok((key, None)) => (vec![Op::Delete { key }], Ok(removed)),
                    Err(error) => (Vec::new(), Err(error)),
                })
            }
        }
    }

    async fn build(&self) -> Result<Snapshot, io::Error> {
        let _applying = self.applying.lock().await;
        let snapshot = self.store.snapshot().await.map_err(io_error)?;
        let last_membership = self.membership().await?;
        Ok(openraft::Snapshot {
            meta: SnapshotMeta {
                last_log_id: snapshot.applied().map(to_raft_id),
                last_membership,
            },
            snapshot,
        })
    }
}

impl RaftSnapshotBuilder<Cfg> for StateAdapter {
    type SnapshotData = Box<dyn StateSnapshot>;

    async fn build_snapshot(&mut self) -> Result<Snapshot, io::Error> {
        self.build().await
    }
}

impl RaftStateMachine<Cfg> for StateAdapter {
    type SnapshotData = Box<dyn StateSnapshot>;
    type SnapshotBuilder = StateAdapter;

    async fn applied_state(&mut self) -> Result<(Option<RaftLogId>, Stored), io::Error> {
        let applied = self.store.applied().await.map_err(io_error)?;
        Ok((applied.map(to_raft_id), self.membership().await?))
    }

    async fn apply<S>(&mut self, mut entries: S) -> Result<(), io::Error>
    where
        S: futures_util::Stream<Item = Result<EntryResponder<Cfg>, io::Error>>
            + Unpin
            + OptionalSend,
    {
        while let Some(item) = entries.next().await {
            let (entry, responder) = item?;
            let _applying = self.applying.lock().await;
            let (ops, answer) = self.decide(&entry.log_id, entry.payload).await?;
            let batch = ApplyBatch {
                log_id: to_alloy_id(&entry.log_id),
                ops,
            };
            self.store.apply(batch).await.map_err(io_error)?;
            if let Some(responder) = responder {
                responder.send(answer);
            }
        }
        Ok(())
    }

    async fn get_snapshot_builder(&mut self) -> StateAdapter {
        self.clone()
    }

    async fn install_snapshot(
        &mut self,
        _meta: &SnapshotMeta<LeaderId<u64, u128>, Membership>,
        snapshot: Box<dyn StateSnapshot>,
    ) -> Result<(), io::Error> {
        let _applying = self.applying.lock().await;
        let install = self.store.begin_install().await.map_err(io_error)?;
        while let Some(chunk) = snapshot.next_chunk().await.map_err(io_error)? {
            install.write(chunk).await.map_err(io_error)?;
        }
        install.finish().await.map_err(io_error).map(drop)
    }

    async fn get_current_snapshot(&mut self) -> Result<Option<Snapshot>, io::Error> {
        let snapshot = self.build().await?;
        Ok(snapshot.meta.last_log_id.is_some().then_some(snapshot))
    }
}

/// The network of a one-voter group: there is no other member to reach.
#[derive(Clone, Copy, Debug)]
struct NoPeers;

impl RaftNetworkFactory<Cfg> for NoPeers {
    type Network = NoPeers;

    async fn new_client(&mut self, _target: u128, _node: &EmptyNode) -> NoPeers {
        NoPeers
    }
}

fn unreachable() -> Unreachable<Cfg> {
    Unreachable::from_string("a one-voter group has no peers (ARFC 0006 section 2.8)")
}

impl RaftNetworkV2<Cfg> for NoPeers {
    type SnapshotData = Box<dyn StateSnapshot>;

    async fn append_entries(
        &mut self,
        _rpc: AppendEntriesRequest<Cfg>,
        _option: RPCOption,
    ) -> Result<AppendResult<Cfg>, RPCError<Cfg>> {
        Err(RPCError::Unreachable(unreachable()))
    }

    async fn vote(
        &mut self,
        _rpc: VoteRequest<Cfg>,
        _option: RPCOption,
    ) -> Result<VoteResponse<Cfg>, RPCError<Cfg>> {
        Err(RPCError::Unreachable(unreachable()))
    }

    async fn full_snapshot(
        &mut self,
        _vote: RaftVote,
        _snapshot: Snapshot,
        _cancel: impl Future<Output = ReplicationClosed> + OptionalSend + 'static,
        _option: RPCOption,
    ) -> Result<SnapshotResponse<Cfg>, StreamingError<Cfg>> {
        Err(StreamingError::Unreachable(unreachable()))
    }
}

/// Why a group operation failed.
#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
pub enum GroupError {
    /// The write was refused and has no effect (clause 3.12.2).
    #[error(transparent)]
    Resource(#[from] ResourceError),
    /// The group could not answer. A write answered so may take effect or
    /// not, now or after a restart (clause 3.12.2).
    #[error("the group is unavailable: {0}")]
    Unavailable(Detail),
    /// A read of the state store failed.
    #[error(transparent)]
    Store(#[from] StoreError),
}

fn unavailable(error: impl fmt::Display) -> GroupError {
    GroupError::Unavailable(Detail::new(&error.to_string()))
}

/// One consensus group on one member: linearizable writes and reads of
/// resources over one `openraft` group.
pub struct Group {
    raft: Raft<Cfg, StateAdapter>,
    pub(crate) scope: RtScope,
    me: NodeId,
    state: Arc<dyn StateStore>,
    /// The soft state of this member's tenure as leader (section 2.3).
    pub(crate) soft: std::sync::Mutex<Soft>,
}

impl fmt::Debug for Group {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("Group")
            .field("me", &self.me)
            .finish_non_exhaustive()
    }
}

impl Group {
    /// Opens the group on member `me` over its stores (clause 3.12.3): starts
    /// `openraft` on them, then waits until every entry the log held at open
    /// is applied, so the group serves nothing older. A store that was never
    /// written opens empty; [`Group::initialize`] then makes it a group.
    ///
    /// The group takes a snapshot after each 10,000 applied entries and purges
    /// the log through it (clause 3.12.4).
    ///
    /// # Errors
    ///
    /// [`GroupError::Store`] when a store cannot be read, and
    /// [`GroupError::Unavailable`] when `openraft` does not start or the log
    /// is not applied within 60 seconds.
    pub async fn open(
        runtime: &Runtime,
        me: NodeId,
        log: Arc<dyn LogStore>,
        state: Arc<dyn StateStore>,
    ) -> Result<Group, GroupError> {
        let last = log.state().await?.last;
        let config = Config {
            cluster_name: "alloy".to_owned(),
            snapshot_policy: SnapshotPolicy::LogsSinceLast(SNAPSHOT_EVERY),
            max_in_snapshot_log_to_keep: 0,
            ..Config::default()
        };
        let config = Arc::new(config.validate().map_err(unavailable)?);
        let adapter = StateAdapter {
            store: Arc::clone(&state),
            applying: Arc::new(Mutex::new(())),
        };
        let scope = RtScope::new(runtime);
        let raft = scoped(
            &scope,
            Raft::new(raft_node(me), config, NoPeers, LogAdapter(log), adapter),
        )
        .await;
        let raft = match raft {
            Ok(raft) => raft,
            Err(error) => {
                end_tasks(&scope).await?;
                return Err(unavailable(error));
            }
        };
        let group = Group {
            raft,
            scope,
            me,
            state,
            soft: std::sync::Mutex::new(Soft::default()),
        };
        if let Some(last) = last {
            let wait = group.raft.wait(Some(OPEN_TIMEOUT));
            let ready = wait.metrics(
                |m| {
                    m.last_applied.as_ref().map(|id| id.index) >= Some(last.index)
                        && m.current_leader.is_some()
                },
                "open",
            );
            if let Err(error) = scoped(&group.scope, ready).await {
                // Releases the stores before returning, as `shutdown` does.
                group.shutdown().await?;
                return Err(unavailable(error));
            }
            group.begin_serving();
        }
        Ok(group)
    }

    /// Makes a never-written group a group with this member as its one voter
    /// (clause 3.12.1).
    ///
    /// # Errors
    ///
    /// [`GroupError::Unavailable`] when the group holds a vote or entries
    /// already, or `openraft` fails.
    pub async fn initialize(&self) -> Result<(), GroupError> {
        let me = raft_node(self.me);
        let raft = &self.raft;
        scoped(&self.scope, async move {
            raft.initialize(BTreeSet::from([me]))
                .await
                .map_err(unavailable)?;
            let wait = raft.wait(Some(OPEN_TIMEOUT));
            wait.current_leader(me, "initialize")
                .await
                .map(drop)
                .map_err(unavailable)
        })
        .await?;
        self.begin_serving();
        Ok(())
    }

    /// Starts this member's tenure as the group's one voter and leader: from
    /// now it serves the group (clause 3.12.3), holding no heartbeat times
    /// (clause 3.15).
    fn begin_serving(&self) {
        let now = self.scope.runtime.clock().now();
        self.soft
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .serve(now);
    }

    /// Proposes `command` once it passes every check that does not need the
    /// state, so that no client input reaches a store call that fails. A
    /// create or update of kind `Node` must hold a [`Node`] that passes the
    /// checks of [`Group::create_node`], so the group never holds a `Node`
    /// that does not decode as one (section 2.6).
    async fn write(&self, command: Command) -> Result<Option<Resource>, GroupError> {
        check_command(&command)?;
        if let Command::Create(resource) | Command::Update(resource) = &command
            && resource.kind == NODE_KIND
        {
            Node::from_resource(resource)
                .ok_or(ResourceError::Invalid(
                    "a Node resource must hold a Node spec and status in the cluster scope",
                ))?
                .check()?;
        }
        let response = scoped(&self.scope, self.raft.client_write(command))
            .await
            .map_err(unavailable)?;
        Ok(response.data?)
    }

    async fn linearize(&self) -> Result<(), GroupError> {
        scoped(
            &self.scope,
            self.raft.ensure_linearizable(ReadPolicy::ReadIndex),
        )
        .await
        .map(drop)
        .map_err(unavailable)
    }

    /// Creates a resource with a new random `uid` and generation 1.
    ///
    /// # Errors
    ///
    /// [`ResourceError::AlreadyExists`] when one of that kind, scope, and
    /// name exists, [`ResourceError::Invalid`] for an unacceptable part or a
    /// resource over [`MAX_RESOURCE`](crate::MAX_RESOURCE) encoded, and
    /// [`GroupError::Unavailable`].
    pub async fn create(
        &self,
        kind: &str,
        scope: Scope,
        name: &str,
        spec: Vec<u8>,
        labels: BTreeMap<String, String>,
    ) -> Result<Resource, GroupError> {
        let target = Target {
            kind: kind.to_owned(),
            scope,
            name: name.to_owned(),
        };
        self.create_with_status(target, spec, Vec::new(), labels)
            .await
    }

    /// [`Group::create`] with a status.
    pub(crate) async fn create_with_status(
        &self,
        target: Target,
        spec: Vec<u8>,
        status: Vec<u8>,
        labels: BTreeMap<String, String>,
    ) -> Result<Resource, GroupError> {
        let Target { kind, scope, name } = target;
        let mut uid = [0; 16];
        self.scope
            .runtime
            .rng()
            .fill(&mut uid)
            .map_err(unavailable)?;
        let resource = Resource {
            kind,
            uid: Uid(uid),
            name,
            scope,
            generation: 1,
            spec,
            status,
            labels,
            deleting: false,
        };
        crate::resource::key(&resource.target())?;
        self.write(Command::Create(resource))
            .await?
            .ok_or_else(|| unavailable("no resource"))
    }

    /// Reads one resource, linearizably (clause 3.1).
    ///
    /// # Errors
    ///
    /// [`ResourceError::Invalid`] for an unacceptable part,
    /// [`GroupError::Store`], and [`GroupError::Unavailable`].
    pub async fn get(
        &self,
        kind: &str,
        scope: &Scope,
        name: &str,
    ) -> Result<Option<Resource>, GroupError> {
        let target = Target {
            kind: kind.to_owned(),
            scope: scope.clone(),
            name: name.to_owned(),
        };
        let key = crate::resource::key(&target)?;
        self.linearize().await?;
        match self.state.get(&key).await? {
            None => Ok(None),
            Some(bytes) => decode_resource(&bytes).map(Some).ok_or_else(|| {
                StoreError::Corrupt(Detail::new("a stored resource does not decode")).into()
            }),
        }
    }

    /// Reads every resource of `kind`, linearizably, in key order: by scope,
    /// then by name.
    ///
    /// # Errors
    ///
    /// As [`Group::get`].
    pub async fn list(&self, kind: &str) -> Result<Vec<Resource>, GroupError> {
        let prefix = kind_prefix(kind)?;
        let mut end = prefix.clone();
        // The prefix ends with a zero byte, which this raises to one.
        if let Some(last) = end.last_mut() {
            *last = 1;
        }
        self.linearize().await?;
        let mut out = Vec::new();
        let mut start = prefix;
        loop {
            let pairs = self.state.scan(&start, Some(&end), MAX_SCAN).await?;
            let Some((last, _)) = pairs.last() else { break };
            start = last.clone();
            start.push(0);
            for (_, value) in pairs {
                out.push(decode_resource(&value).ok_or_else(|| {
                    StoreError::Corrupt(Detail::new("a stored resource does not decode"))
                })?);
            }
        }
        Ok(out)
    }

    /// Replaces the spec, status, and labels of the resource of `resource`'s
    /// kind, scope, and name when its generation is `resource.generation`,
    /// and returns it with its generation one more (clause 3.5).
    ///
    /// # Errors
    ///
    /// [`ResourceError::NotFound`], [`ResourceError::Conflict`],
    /// [`ResourceError::Invalid`] as for [`Group::create`], and
    /// [`GroupError::Unavailable`].
    pub async fn update(&self, resource: Resource) -> Result<Resource, GroupError> {
        self.write(Command::Update(resource))
            .await?
            .ok_or_else(|| unavailable("no resource"))
    }

    /// Marks a resource `deleting` when its generation is `generation`, the
    /// first phase of clause 3.6, and returns it.
    ///
    /// # Errors
    ///
    /// As [`Group::update`].
    pub async fn delete(
        &self,
        kind: &str,
        scope: &Scope,
        name: &str,
        generation: u64,
    ) -> Result<Resource, GroupError> {
        let target = Target {
            kind: kind.to_owned(),
            scope: scope.clone(),
            name: name.to_owned(),
        };
        self.write(Command::Delete { target, generation })
            .await?
            .ok_or_else(|| unavailable("no resource"))
    }

    /// Removes a `deleting` resource when its generation is `generation`, the
    /// last phase of clause 3.6. Removing a node drops the soft state held
    /// for it (section 2.3).
    ///
    /// # Errors
    ///
    /// [`ResourceError::NotDeleting`], and as [`Group::update`].
    pub async fn remove(
        &self,
        kind: &str,
        scope: &Scope,
        name: &str,
        generation: u64,
    ) -> Result<(), GroupError> {
        let target = Target {
            kind: kind.to_owned(),
            scope: scope.clone(),
            name: name.to_owned(),
        };
        let removed = self.write(Command::Remove { target, generation }).await?;
        if let Some(node) = removed.filter(|r| r.kind == NODE_KIND) {
            self.soft
                .lock()
                .unwrap_or_else(std::sync::PoisonError::into_inner)
                .forget(node.uid);
        }
        Ok(())
    }

    /// Takes a snapshot now and purges the log through it, as clause 3.12.4
    /// does after 10,000 entries, and returns its position.
    ///
    /// # Errors
    ///
    /// [`GroupError::Unavailable`] when `openraft` fails.
    pub async fn snapshot(&self) -> Result<Option<LogId>, GroupError> {
        let raft = &self.raft;
        scoped(&self.scope, async move {
            let Some(applied) = raft.metrics().borrow_watched().last_applied else {
                return Ok(None);
            };
            raft.trigger().snapshot().await.map_err(unavailable)?;
            let wait = raft.wait(Some(OPEN_TIMEOUT));
            let snapshot = Metric::Snapshot(Some(applied));
            wait.ge(snapshot, "snapshot").await.map_err(unavailable)?;
            let purged = Metric::Purged(Some(applied));
            wait.ge(purged, "purge").await.map_err(unavailable)?;
            Ok(Some(to_alloy_id(&applied)))
        })
        .await
    }

    /// Stops the group: shuts `openraft` down, then ends every task the
    /// group started and waits until each has ended, also after a store
    /// failure. When it returns, nothing of the group holds the stores, so
    /// they can be built again at once.
    ///
    /// # Errors
    ///
    /// [`GroupError::Unavailable`] when a task ended in a panic, or the
    /// group's tasks did not end within 60 seconds.
    pub async fn shutdown(self) -> Result<(), GroupError> {
        let stopped = scoped(&self.scope, self.raft.shutdown()).await;
        let ended = end_tasks(&self.scope).await;
        stopped.map_err(unavailable)?;
        ended
    }
}

/// Dropping a group without [`Group::shutdown`] ends its tasks without
/// waiting for them: the stores are released once each has been dropped.
impl Drop for Group {
    fn drop(&mut self) {
        self.scope.tasks.close();
    }
}

/// Ends every task of `scope` and waits until each has ended, at most 60
/// seconds.
async fn end_tasks(scope: &RtScope) -> Result<(), GroupError> {
    scope.tasks.close();
    scope
        .runtime
        .timeout(STOP_TIMEOUT, scope.tasks.ended())
        .await
        .map_err(|_| unavailable("the group's tasks did not end within 60 seconds"))
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::store::MAX_ENTRY_PAYLOAD;

    fn entry(index: u64, len: usize) -> Entry {
        Entry {
            log_id: LogId {
                term: 1,
                leader: NodeId([1; 16]),
                index,
            },
            payload: vec![0; len],
        }
    }

    #[test]
    fn appends_split_into_runs_a_store_accepts() {
        assert!(append_runs(Vec::new()).is_empty());
        let small: Vec<Entry> = (1..=3).map(|i| entry(i, 10)).collect();
        assert_eq!(append_runs(small.clone()), vec![small]);
        // Two entries of the largest payload exceed MAX_APPEND_BYTES.
        let large: Vec<Entry> = (1..=3).map(|i| entry(i, MAX_ENTRY_PAYLOAD)).collect();
        let runs = append_runs(large.clone());
        assert_eq!(runs.len(), 3);
        for run in &runs {
            let bytes: usize = run.iter().map(|e| e.payload.len()).sum();
            assert!(bytes <= MAX_APPEND_BYTES);
        }
        assert_eq!(runs.concat(), large);
    }
}
