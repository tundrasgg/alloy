# ARFC 0006: Control plane

- Status: accepted
- Depends on: 0001, 0002, 0004, 0005, 0007, 0008, 0017, 0018
- Crates: alloy-state, alloy-ctl
- Decisions: 0019, 0031, 0064

## 1. Purpose

Holds the desired and observed state of the cluster, replicated and consistent,
and drives the world toward the desired state.

## 2. Model

### 2.1 Resources

All state is **resources**. A resource has `kind`, `uid` (128-bit, random),
`name`, `scope` (cluster, or org and project), `generation`, `spec` (what is
wanted), `status` (what is), `labels`, and `deleting`. Kinds and their schemas are
Protocol Buffers messages (ARFC 0008).

### 2.2 Groups

| Group | Holds | One per |
|---|---|---|
| global | Orgs, projects, principals, policies, regions, cluster settings, node records | cluster |
| regional | Everything else | region |

Each group is a Raft consensus group among the control nodes of its scope. A
one-region cluster runs both groups on the same nodes.

### 2.3 Soft state

Heartbeats, path measurements, usage samples, and metrics do not go through
consensus. Control nodes hold them in memory with leases and rebuild them after a
restart.

### 2.4 Controllers

A controller watches kinds and reconciles: it compares `spec` and `status` and
acts. Controllers are level triggered and idempotent. Exactly one instance of each
controller is active per group, chosen by a lease in the group.

### 2.5 Views

A node agent subscribes to a **view**: the subset of resources it needs. A view is
defined by a query over the node's identity (its guests, their VPCs and volumes,
the peers hosting the other ends). Every view also holds the node's own record and
the records of the nodes that hold `anchor` or `control`, through which the node
reaches the control plane (clause 3.13).

### 2.6 Nodes

Node records (ARFC 0002 section 2) are resources of kind `Node` in service
`cluster`, with cluster scope, held by the global group. `ListNodes` and `GetNode`
return one `Node` for each node that holds a record and one for each `pending`
join request (ARFC 0002 section 3.3). Besides the fields of section 2.1, a `Node`
has these, spelled in JSON as ARFC 0008 section 2 says:

| Field | Part | Value |
|---|---|---|
| `roles` | spec | Its roles, each one of `anchor`, `control`, `compute`, `storage`. Empty for a client node |
| `zone` | spec | Its zone |
| `advertise` | spec | Its advertise addresses (ARFC 0002 section 3.10), in record order |
| `state` | status | `pending`, `joining`, `active`, or `unreachable`, computed when read (clause 3.15) |
| `node_id` | status | As ARFC 0002 section 2 defines it |
| `record_serial`, `record_not_after` | status | Of its current record. Unset for a `pending` node |
| `reported` | status | Whether the control plane has accepted a heartbeat from it since its first record was issued (clause 3.14). Committed state |
| `unreachable_since` | status | The leader's wall time when the node last became `unreachable`, set and cleared as clause 3.16 says. Committed state, unset while the node is not `unreachable` |
| `running_release` | status | ARFC 0017 clause 3.11. Unset while none is recorded |
| `addresses` | status | The addresses of the latest heartbeat the leader accepted (clause 3.13). Soft state, empty when there is none |
| `last_heartbeat_time` | status | The leader's wall time when it accepted that heartbeat. Soft state, unset when there is none |
| `mesh` | status | `direct` or `relay-only` (ARFC 0004 section 3.7) while the control plane holds the node's path class, unset otherwise |

A `Node` carries no key, secret, token, or token id.

### 2.7 Stores

Each group is a Raft log and a state machine (ADR 0064). On each member, the Raft
log and vote are in one instance of the seam `log-store` and the applied state in
one instance of the seam `state-store` (section 5). A member's Raft id is its
`node_id`.

The bindings of both seams are node-local configuration, never control plane
state, so ARFC 0001 clause 3.13 does not apply to them. On `os`, `alloyd` binds
`native` with `dir` set to `<data-dir>/state/global` for the global group and
`<data-dir>/state/region-<region name>` for a regional group. It creates each
directory with mode `0700` on Linux, and on Windows with the access control list
it inherits from the data directory (ARFC 0002 section 3.12). Simulated hosts bind
`mem`. A node that is a member of no group has no `state` directory.

### 2.8 First deployment

M11 rung 2 builds this much of this ARFC:

- Sections 2.1, 2.2 with the one region `default`, 2.3 for heartbeats, 2.4 for
  the node lifecycle controller of clause 3.16, 2.6, and 2.7.
- Clauses 3.1 without stale reads, 3.2 with one control node and one voter, 3.5,
  3.6 for nodes, 3.10 without backups, and 3.12 to 3.16.
- Sections 4 and 5, section 6 for one voter, section 7 except the 6 GiB warning
  and the watch limit, and section 8 except authentication between control nodes.

Everything else is built by later tasks: views and watches (2.5, 3.7, 3.8, 3.9),
learners, more than one voter, and membership changes (3.2, 3.3), forwarding
(3.4), stale reads, backups (3.10), and recovery (3.11). With one control node
there is no follower to forward from. Until views exist, a node takes its homes
and the control nodes of clause 3.13 from the bootstrap view of its join (ARFC
0002 section 3.13).

## 3. Behavior

3.1 Writes are linearizable. Reads are linearizable by default and may be stale by
request, with the staleness bound returned (I-11).

3.2 Group membership is automatic. A cluster with 1 or 2 control nodes has 1
voter. With 3 or 4 it has 3 voters. With 5 or more it has 5, spread across zones.
Other control nodes are learners. A cloud admin can pin the voter set.

3.3 Promotion and demotion happen one member at a time, and only when the group is
healthy.

3.4 Every control node serves the API. A follower forwards writes to the leader.

3.5 Optimistic concurrency: a write names the `generation` it read. A mismatch
fails with `Conflict`.

3.6 Deletion is two-phase: `deleting` is set, controllers release what the
resource holds, then the resource is removed. A resource must never remain
`deleting` for more than 5 minutes without an error in its status (I-14).

3.7 A watch delivers every committed change to matching resources in order, and
resumes from a revision after a disconnect. A resume from a compacted revision
must fail with `Compacted` so that the client relists.

3.8 A node must never receive a resource outside its view (I-03). View changes
reach a node within 1 second of commit at the 99th percentile on a healthy
cluster.

3.9 A node that cannot reach the control plane keeps serving its last view. Guests
keep running. Nothing new starts.

3.10 The state store takes a snapshot at least every 10,000 log entries, and a
backup on a schedule (default hourly, kept 7 days) to a local path or a bucket.

3.11 Recovery without quorum is a manual, audited operation:
`alloy cluster recover --from <node>` rebuilds a one-voter group from that node's
state and must print what may have been lost.

3.12 Stores.

3.12.1 `alloy init` (ARFC 0002 section 3.1) creates the global group with its node
as the one voter. It then records the regional group of region `default` in the
global group and creates that group with the same voter, the record coming before
the regional group's first entry (ARFC 0001 clause 3.13).

3.12.2 A write is acknowledged only after its entry is durable in the log stores
of a quorum of voters and applied to the state store of the member that answers.
A write acknowledged before `alloyd` or its machine stops, by a kill or a power
loss, must be visible after `alloyd` starts again (I-11). The outcome of an
error depends on its code:

- A write answered with any error but `Unavailable` must have no effect, then or
  after any restart. A member gives such an error only for a write it has not
  proposed, or whose committed entry its apply turns into no change, as a
  generation mismatch (3.5) does.
- A write answered with `Unavailable`, or not answered, may take effect or not,
  now or once the member restarts (ARFC 0008 section 6). An entry that is
  durable in the log store is kept even when its apply fails (section 6), and
  clause 3.12.3 applies it at the next start. A client learns the outcome by
  reading the resource or by retrying with the same idempotency key.

3.12.3 At start, `alloyd` opens the stores of each group it is a member of,
applies the log entries above the state store's applied position, and only then
serves that group.

3.12.4 The state store is durable on its own, so a snapshot (3.10) is the state
store's contents at an applied position, taken with `StateStore::snapshot`. After
10,000 entries have been applied since a member's last snapshot of a group, it
takes one and purges that group's log through the snapshot's position. Once the
purge completes, the log store holds no entry at or below that position.

3.13 Heartbeats. A node reports to the control plane with **heartbeats**, the
status messages of section 4.3, carried as node messages on the node channel
(ARFC 0002 section 3.13). Every node in local join state `joined` (ARFC 0002
section 2), a client node included, sends them while it holds a record that has
not expired by its own clock:

- Content. `NodeStatus` holds `addresses`, the node's current addresses, leaving
  out loopback, link-local, and `al*` interfaces as ARFC 0002 section 3.10 does,
  at most 64, IPv4 before IPv6, each family in ascending byte order. Its
  `release` is the node's own release while ARFC 0017 clause 3.11 makes the node
  write its running release, and empty otherwise. A heartbeat names no node: it
  applies to the node whose record holds the mesh key that the session it came
  on authenticated (3.14).
- Schedule. The node sends the first heartbeat within 1 second after `alloyd`
  holds a valid record. A heartbeat is one request of the node channel with a
  time limit of 4 seconds: the node sends it again every 1 second until its
  reply arrives or 4 seconds pass. It succeeds when the reply's result is `OK`.
  After a success the next heartbeat starts 5 seconds after the previous one
  started. After any other result, or no reply, the next starts 1 second after
  the failed one ended, the delay doubling with each further failure in a row
  up to 4 seconds. After `REFUSED` or `RELEASE_REFUSED` the next starts 60
  seconds after the failed one ended.
- Running release. While the node has to write its running release, every
  heartbeat carries `release`. A reply `OK` tells the node the write is applied,
  and `RELEASE_REFUSED` that it is refused, with the finalized release that ARFC
  0017 clause 3.11 makes the node report.
- Path. The node sends each heartbeat on one of its home sessions (ARFC 0004
  section 3.1.2): the one on which its latest heartbeat succeeded, and after a
  failure, when it has two homes, the other one. An anchor that holds `control`
  handles the heartbeat as 3.14 says. An anchor that does not sends it on as
  `forwarded` (ARFC 0002 section 3.13) to a control node: first to the one that
  last answered it with a result other than `UNAVAILABLE`, or the first in
  ascending name order until one has. When that control node answers
  `UNAVAILABLE` or gives no reply within 1.5 seconds, the anchor moves to the
  next control node in ascending name order, wrapping around, and sends the
  heartbeat there. It replies to the node with the first result other than
  `UNAVAILABLE`, or with `UNAVAILABLE` once two control nodes have failed, at
  most 3 seconds after it took the request. A control node that fails stays
  passed over, so successive heartbeats through an anchor try every control node
  in turn.
- Rungs. In M11 rung 2 a home session runs directly over UDP or `stream` to the
  anchor's published address on the underlay (ARFC 0002 section 3.13). From M11
  rung 3, home sessions take the paths of ARFC 0004 and node messages pass
  through `al0`. Heartbeats keep the same bytes, timing, and rules.
- A node that holds `control` makes its own heartbeat in its own process, on
  the same schedule, with no node message, and handles it as 3.14 says.

With 4 control nodes or fewer, a node has a heartbeat accepted less than 30
seconds after the one before, and so shows `active` (3.15), while one of its
homes answers within these limits and either holds `control` and reaches the
leader, or holds sessions with the control nodes, at least one of which reaches
the leader.

3.14 Accepting a heartbeat. A control node that is not the leader of the global
group passes a heartbeat to the leader, as 3.4 passes writes, and replies with
the leader's result, or with `UNAVAILABLE` when that result does not reach it
within 1 second. A leader that does not yet serve the group (3.12.3) replies
`UNAVAILABLE`. The heartbeat's mesh key is the one the session it came on
authenticated, or the one its `forwarded` names. The leader:

- replies `INVALID`, changing nothing, when the message breaks section 4.3;
- replies `REFUSED`, changing nothing, when no node record in the global group
  holds the mesh key, or that node's record has expired by the leader's wall
  clock, so a node with an expired record shows `unreachable` until it renews
  (ARFC 0002 section 3.5);
- when `release` is set and differs from the node's running release, applies it
  as the running release write of ARFC 0017 clause 3.11, with its audit record
  (ARFC 0018 clause 3.3.7). When the write is refused, it replies
  `RELEASE_REFUSED` with `finalized_release` set, changing nothing but that
  audit record. A `release` equal to the running release is a write already
  applied, and commits nothing;
- records as soft state the moment it accepted the heartbeat, on its monotonic
  clock, its wall time, and the heartbeat's `addresses`, replacing what it held
  for the node;
- when the node's `reported` is false, commits `reported` true together with its
  audit record;
- replies `OK` once every commit above is applied. A commit that fails gives
  `UNAVAILABLE`, with the outcome clause 3.12.2 gives that code.

A heartbeat changes no state but these and is not a mutating call for ARFC 0008
clause 3.7 or ARFC 0018 clause 3.3.1. It writes an audit record only with a
commit above. A heartbeat answered `INVALID` or `REFUSED`, or answered
`UNAVAILABLE` before any commit, or dropped by the node channel (ARFC 0002
section 3.13), writes none.

A deleted node holds no record in the global group. Its anchor drops its node
messages once it no longer holds the node's record as valid (ARFC 0002 section
3.13), and the leader refuses those still passed on. So a deleted node's
heartbeats change nothing and write no audit record. They come at most one per
60 seconds once one is refused, and only until its record expires by its own
clock, at most 24 hours after its last renewal (ARFC 0002 section 3.5). Its
renewal is then refused with `node_unknown`, after which it sends nothing more.

3.15 Node state. The control plane **last heard** from a node with `reported`
true at the later of these moments, on the leader's monotonic clock: the
leader's acceptance of the node's latest heartbeat (3.14), and, while the node's
`unreachable_since` is unset, the moment the leader began serving the global
group (3.12.3). A new leader, the same node after `alloyd` restarts included,
holds no heartbeat times (2.3). Counting its own start keeps a node that was
`active` so across the change, and `unreachable_since` keeps a node that was
`unreachable` so. This is what "heard from" means in ARFC 0002 section 2. The
leader computes each node's `state` when it serves the read:

| State | When |
|---|---|
| `pending` | A join request waits for approval. No record exists |
| `joining` | A record exists and `reported` is false |
| `active` | `reported` is true and the control plane last heard from the node less than 30 seconds before the read |
| `unreachable` | `reported` is true and the control plane last heard from the node 30 seconds or more before the read, or has no moment it last heard from it |

- A node whose heartbeats keep being accepted, each less than 30 seconds after
  the one before, must show `active` at every read after the first of them is
  accepted, reads by a new leader included, except while its
  `unreachable_since` is still set (3.16).
- A node from which the leader accepts no heartbeat must show `unreachable` at
  every read from 30 seconds after the control plane last heard from it until
  the leader accepts one.
- A deleted node is absent from `ListNodes`. Deleting a node is how its record
  is revoked (ARFC 0002 section 3.5), so there is no `revoked` state.
- The node list of M11 rung 2 shows a node healthy when its state is `active`.

3.16 Node lifecycle controller. One controller of the global group (2.4) owns the
node record lifecycle:

- It deletes a `joining` node within 60 seconds after its record's `not_after`
  (ARFC 0002 section 6), and a `pending` request when the token it used expires
  (ARFC 0002 section 3.3), each with its audit record (ARFC 0018 clause 3.3.7).
- Within 5 seconds after a node's state becomes `unreachable`, it commits the
  node's `unreachable_since` as the leader's wall time at that moment. Within 5
  seconds after the leader accepts a heartbeat from a node whose
  `unreachable_since` is set, it clears the field. Neither commit writes an
  audit record (ARFC 0018 clause 3.3.1).
- The leader drops a node's soft state when the node is deleted.

## 4. Formats

### 4.1 State export

`StateSnapshot` writes, and `StateInstall` reads, the whole contents of a state
store in this format, all integers big endian:

| Bytes | Field | Value |
|---|---|---|
| 0 to 7 | `magic` | ASCII `ALSTATE1` |
| 8 | `has_applied` | 1 when the store has an applied position, else 0 |
| 9 to 16 | `term` | The applied position's `term`, else 0 |
| 17 to 32 | `leader` | The applied position's `leader`, else 16 zero bytes |
| 33 to 40 | `index` | The applied position's `index`, else 0 |

Then one record per key, in ascending bytewise key order: `key_len` (4 bytes, 1
to 4,096), the key, `value_len` (4 bytes, 0 to 2,097,152), the value. Then the
trailer: 4 zero bytes, `count` (8 bytes, the number of records), and `digest`
(32 bytes, BLAKE3 over every byte before it).

The export of given contents is unique, so two stores with equal contents and
applied positions, of any implementation, export identical bytes. An install
fails with `Malformed` and changes nothing when the bytes end early, `magic`
differs, `has_applied` is neither 0 nor 1, `has_applied` is 0 and bytes 9 to 40
are not all zero, a length is out of range, keys are not in strictly ascending
order, `count` differs from the records read, `digest` does not match, or any
byte follows `digest`.

### 4.2 Store files

A `native` log store keeps one file, `log.redb`, and a `native` state store one
file, `state.redb`, in its `dir`. Both are `redb` 4.5.0 database files (ADR
0064). Their contents are not specified further and are not a stable format.

### 4.3 Status messages

The status messages take numbers 16 and 17 of the `body` of `NodeMessage` (ARFC
0002 section 3.13), in the same file and package. Numbers 18 to 31 stay reserved
for later status messages.

```proto
// In message NodeMessage, oneof body:
//   NodeStatus node_status = 16;              // node to anchor, anchor to control node
//   NodeStatusReply node_status_reply = 17;   // answers node_status
message NodeStatus {
  repeated bytes addresses = 1;      // 4 or 16 bytes each, 0 to 64, clause 3.13 order
  string release = 2;                // empty, or as JoinHello.release, at most 64 bytes
}
enum StatusResult {
  STATUS_RESULT_UNSPECIFIED = 0; STATUS_RESULT_OK = 1; STATUS_RESULT_UNAVAILABLE = 2;
  STATUS_RESULT_INVALID = 3; STATUS_RESULT_REFUSED = 4; STATUS_RESULT_RELEASE_REFUSED = 5;
}
message NodeStatusReply {
  StatusResult result = 1;
  string finalized_release = 2;      // set only with STATUS_RESULT_RELEASE_REFUSED
}
```

Clauses 3.13 and 3.14 write the results without the prefix `STATUS_RESULT_`. A
`NodeStatus` breaks this section when it holds more than 64 addresses, an
address of another length, a `release` over 64 bytes, or a non-empty `release`
that is not spelled as the `release` of `JoinHello` (ARFC 0002 section 3.13).

## 5. Seams

| Kind | Trait | Marker | Implementations | Suite |
|---|---|---|---|---|
| `log-store` | `LogStore` | `LogStoreSeam` | `native`, `mem` | `tests/conformance/log-store/` |
| `state-store` | `StateStore` | `StateStoreSeam` | `native`, `mem` | `tests/conformance/state-store/` |

Both have swap class `restart` and context `alloy_rt::Runtime`. Raft is `openraft`,
and `native` is built on `redb` (ADR 0064).

### 5.1 Interface

The items below are the public interface of the two seams in `alloy-state`. Error
types follow the rules of ARFC 0001 section 2.1. Types that carry stored bytes
implement `Debug` by hand and print lengths only, never contents (I-22).

```rust
use alloy_seam::{BoxFuture, Detail, RegistryBuilder, RegistryError};
use std::fmt;

pub const MAX_ENTRY_PAYLOAD: usize = 67_174_400;
pub const MAX_APPEND_BYTES: usize = 134_217_728;
pub const MAX_READ_BYTES: usize = 67_108_864;
pub const MAX_KEY: usize = 4_096;
pub const MAX_VALUE: usize = 2_097_152;
pub const MAX_BATCH_BYTES: usize = 67_108_864;
pub const MAX_SCAN: usize = 10_000;
pub const MAX_CHUNK: usize = 1_048_576;

/// A member's Raft id: its `node_id` (ARFC 0002 section 2).
#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub struct NodeId(pub [u8; 16]);

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Vote {
    pub term: u64,
    pub node: NodeId,
    /// Granted by a quorum.
    pub committed: bool,
}

/// The position of a log entry: the term and leader that wrote it, and its
/// index.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub struct LogId {
    pub term: u64,
    pub leader: NodeId,
    pub index: u64,
}

/// One log entry. The store never interprets `payload`.
#[derive(Clone, PartialEq, Eq)]
pub struct Entry {
    pub log_id: LogId,
    pub payload: Vec<u8>,
}

/// Writes `Entry { log_id: <log_id as Debug>, len: <payload.len()> }`.
impl fmt::Debug for Entry { /* ... */ }

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct LogState {
    /// The highest position purged, `None` before the first purge.
    pub last_purged: Option<LogId>,
    /// The position of the highest entry held, else `last_purged`.
    pub last: Option<LogId>,
}

/// Seam `log-store`: one group's Raft log and vote on one member.
pub trait LogStore: Send + Sync + fmt::Debug + 'static {
    fn read_vote(&self) -> BoxFuture<'_, Result<Option<Vote>, StoreError>>;
    fn save_vote(&self, vote: Vote) -> BoxFuture<'_, Result<(), StoreError>>;
    fn state(&self) -> BoxFuture<'_, Result<LogState, StoreError>>;
    fn read(&self, first: u64, last: u64) -> BoxFuture<'_, Result<Vec<Entry>, StoreError>>;
    fn append(&self, entries: Vec<Entry>) -> BoxFuture<'_, Result<(), StoreError>>;
    fn truncate_from(&self, index: u64) -> BoxFuture<'_, Result<(), StoreError>>;
    fn purge_through(&self, log_id: LogId) -> BoxFuture<'_, Result<(), StoreError>>;
}

#[derive(Clone, PartialEq, Eq)]
pub enum Op {
    Put { key: Vec<u8>, value: Vec<u8> },
    Delete { key: Vec<u8> },
}

/// Writes `Put { key_len: <n>, value_len: <n> }` or `Delete { key_len: <n> }`.
impl fmt::Debug for Op { /* ... */ }

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct ApplyBatch {
    pub log_id: LogId,
    pub ops: Vec<Op>,
}

/// Seam `state-store`: one group's applied state on one member, an ordered
/// map from byte keys to byte values with an applied position.
pub trait StateStore: Send + Sync + fmt::Debug + 'static {
    fn applied(&self) -> BoxFuture<'_, Result<Option<LogId>, StoreError>>;
    fn apply(&self, batch: ApplyBatch) -> BoxFuture<'_, Result<(), StoreError>>;
    fn get<'a>(&'a self, key: &'a [u8])
        -> BoxFuture<'a, Result<Option<Vec<u8>>, StoreError>>;
    /// `end` `None` means no upper bound.
    fn scan<'a>(&'a self, start: &'a [u8], end: Option<&'a [u8]>, limit: usize)
        -> BoxFuture<'a, Result<Vec<(Vec<u8>, Vec<u8>)>, StoreError>>;
    fn snapshot(&self) -> BoxFuture<'_, Result<Box<dyn StateSnapshot>, StoreError>>;
    fn begin_install(&self) -> BoxFuture<'_, Result<Box<dyn StateInstall>, StoreError>>;
}

/// A read-only view of a whole state store at one applied position.
pub trait StateSnapshot: Send + Sync + fmt::Debug {
    fn applied(&self) -> Option<LogId>;
    /// The next 1 to `MAX_CHUNK` bytes of the export (section 4.1), `None`
    /// after the last.
    fn next_chunk(&self) -> BoxFuture<'_, Result<Option<Vec<u8>>, StoreError>>;
}

/// Receives an export in chunks of any size. Dropped before `finish`, it
/// leaves the store unchanged.
pub trait StateInstall: Send + Sync + fmt::Debug {
    fn write(&self, chunk: Vec<u8>) -> BoxFuture<'_, Result<(), StoreError>>;
    /// Returns the export's applied position.
    fn finish(self: Box<Self>) -> BoxFuture<'static, Result<Option<LogId>, StoreError>>;
}

#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
pub enum StoreError {
    #[error("invalid input: {0}")]
    InvalidInput(Detail),
    #[error("{what} is {len} bytes, the limit is {limit}")]
    TooLarge { what: Detail, len: u64, limit: u64 },
    #[error("the entries do not continue the log")]
    NotContiguous,
    #[error("index {index} is not above the applied index {applied}")]
    Stale { index: u64, applied: u64 },
    #[error("an install is in progress")]
    Busy,
    #[error("the export is malformed: {0}")]
    Malformed(Detail),
    #[error("stored data is corrupt: {0}")]
    Corrupt(Detail),
    #[error("storage failed: {0}")]
    Io(Detail),
}

/// Seam markers (ARFC 0001 section 2.1), with `Context = alloy_rt::Runtime`
/// and `SWAP = SwapClass::Restart`.
#[derive(Debug)] pub struct LogStoreSeam;   // Object = dyn LogStore,   KIND = "log-store"
#[derive(Debug)] pub struct StateStoreSeam; // Object = dyn StateStore, KIND = "state-store"

/// Registers `native` for both kinds, with their configuration types.
pub fn register_native(builder: &mut RegistryBuilder) -> Result<(), RegistryError>;
/// Registers `mem` for both kinds, with their configuration types. Each call
/// creates one table of contents shared by the instances its factories build.
pub fn register_mem(builder: &mut RegistryBuilder) -> Result<(), RegistryError>;
```

### 5.2 Configuration

Payloads are Protocol Buffers messages (ARFC 0001 section 4). Both
implementations read and write version 1 only and ignore unknown fields.

| Kinds | Implementation | Message |
|---|---|---|
| `log-store`, `state-store` | `native` | `message NativeStoreConfig { string dir = 1; }` |
| `log-store`, `state-store` | `mem` | `message MemStoreConfig { string name = 1; uint64 fail_after = 2; }` |

`dir` is the absolute native path of an existing directory. `native` creates its
file there (section 4.2) when it is missing, with mode `0600` on Linux, and on
Windows with the access control list it inherits from `dir`. While an instance
lives, building another instance of the same kind on the same `dir`, in any
process, fails with `BuildError::Failed`.

`name` is 1 to 64 bytes of `[a-z0-9-]`. Instances of one kind built through one
`register_mem` call with the same `name` share contents, which outlive every
instance, so building a new instance after dropping one models a restart. While
one lives, building another of the same kind and `name` fails with
`BuildError::Failed`. With `fail_after` above 0, once that many mutating calls
(`save_vote`, `append`, `truncate_from`, `purge_through`, `apply`, `finish`) have
succeeded on the shared contents, every later mutating call fails with `Io` and
changes nothing.

A missing or invalid `dir` or `name` fails the build with
`BuildError::Config(ConfigError::Invalid { .. })` naming the field.

### 5.3 Log store

5.3.1 A store never written holds no vote, no entry, and `LogState { last_purged:
None, last: None }`.

5.3.2 Every mutating call is atomic and durable when it returns `Ok`: a new
instance built on the same `dir` or `name` holds its effect, and a process killed
during the call leaves the store as before it or as after it.

5.3.3 `save_vote` replaces the vote, and `read_vote` returns the last one saved.

5.3.4 `append` adds its entries in order. An empty list changes nothing. Each
entry's index must be one more than the previous entry's, and the first's one
more than `last`'s index when `last` is set; otherwise the call fails with
`NotContiguous`. An entry whose payload exceeds `MAX_ENTRY_PAYLOAD`, or a list
whose payloads together exceed `MAX_APPEND_BYTES`, fails with `TooLarge`. A
failed call changes nothing.

5.3.5 `read(first, last)` fails with `InvalidInput` when `first > last`. Otherwise
it returns the held entries with index from `first` to `last`, ascending, each as
appended, stopping after the entry that brings the payload total to
`MAX_READ_BYTES` or more.

5.3.6 `truncate_from(index)` removes every entry with an index at or above
`index`, and `last` becomes the position of the highest entry left, else
`last_purged`. It fails with `InvalidInput`, changing nothing, when `index` is at
or below `last_purged`'s index.

5.3.7 `purge_through(log_id)` removes every entry with an index at or below
`log_id.index` and sets `last_purged` to `log_id`. When `log_id.index` is above
`last`'s index, `last` becomes `log_id`. A `log_id` at or below `last_purged`'s
index changes nothing.

5.3.8 After a call fails with `Io`, every later call on that instance fails with
`Io`.

### 5.4 State store

5.4.1 A store never written holds no key and no applied position.

5.4.2 `apply` and `StateInstall::finish` are atomic and durable as 5.3.2 says.

5.4.3 `apply` performs the batch's ops in order, `Put` setting a key's value and
`Delete` removing the key if present, and sets the applied position to the
batch's `log_id`. An empty `ops` only sets the position. It fails, changing
nothing, with `Stale` when `log_id.index` is at or below the applied index, with
`InvalidInput` for an empty key, with `TooLarge` for a key above `MAX_KEY`, a
value above `MAX_VALUE`, or keys and values together above `MAX_BATCH_BYTES`, and
with `Busy` while an install is in progress.

5.4.4 `get` and `scan` reflect every `apply` that returned before the call
started. `scan` returns the pairs whose key is at or after `start` and, when
`end` is set, before `end`, ascending bytewise, at most `limit` of them, stopping
after the pair that brings key and value bytes to `MAX_READ_BYTES` or more. A
`limit` of 0 or above `MAX_SCAN` fails with `InvalidInput`.

5.4.5 `snapshot` returns a view of the store as of the call: `apply` calls after
it do not change what the view exports. Its chunks, joined, are the export of
section 4.1.

5.4.6 `begin_install` fails with `Busy` while another install is in progress.
`finish` checks the export as section 4.1 says, then replaces the store's whole
contents and applied position with the export's. A malformed export fails with
`Malformed` and changes nothing. Until `finish` returns, reads see the contents
from before the install.

5.4.7 After a call fails with `Io`, every later call on that instance fails with
`Io`.

### 5.5 Suites

The suites are generic over the traits and run against `mem` and, on the CI host
in a temporary directory, against `native`. They check sections 4.1 and 5.2 to
5.4. They include an export from each implementation installed into the other,
which must give equal contents and an identical export, and, for `native`, a
child process killed during a stream of appends, after which the log holds every
acknowledged entry and nothing that breaks 5.3.4. `native` is not run on the
simulator (ADR 0064).

## 6. Failure behavior

A minority of voters lost: no effect on clients beyond a leader change, complete
within 5 seconds. Quorum lost: all writes fail with `Unavailable`, stale reads
continue. A controller that crashes mid-action must be safe to rerun from the
start.

A store call that fails with `Io` or `Corrupt` stops its member serving that
group: calls that need the group fail with `Unavailable` from that member, and
`alloyd` logs the error every 60 seconds and does not reopen the stores until it
restarts. With one voter, that is the whole group.

A heartbeat that fails is retried as clause 3.13 says, and its node shows
`unreachable` once 30 seconds pass after the control plane last heard from it
(clause 3.15).

## 7. Limits

1 MiB per resource. 8 GiB of state per group, with a warning at 6 GiB. 10,000
watches per control node. 64 MiB per write batch.

Store limits are the constants of section 5.1, each failing as sections 5.3 and
5.4 say. 64 addresses per heartbeat: one with more is answered `INVALID` and
changes nothing (section 4.3). The leader holds heartbeat soft state for at most
one entry per node record. An anchor holds, per control node, only whether it is
the one to try first (clause 3.13).

## 8. Security

Control nodes authenticate each other by node record. Views enforce I-03. All
mutations pass authorization (ARFC 0007) and audit (ARFC 0018) before they are
proposed, except the node lifecycle controller's commits of `unreachable_since`,
which record what the leader observed and write no audit record (clause 3.16).
A heartbeat applies only to the node whose record holds the mesh key its AL
session authenticated, and changes only that node's soft state, its `reported`
flag, and its running release (ARFC 0020 clause 3.3).
Store files are readable only as the data directory is (ARFC 0002 section 3.12).
Upholds I-03, I-11, I-14, I-15, I-21.

## 9. Open questions

- Cross-region replication of the global group and its latency budget. Research
  task to be cut in M2.
