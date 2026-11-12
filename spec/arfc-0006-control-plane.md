# ARFC 0006: Control plane

- Status: accepted
- Depends on: 0001, 0002, 0005
- Crates: alloy-state, alloy-ctl
- Decisions: 0019, 0031

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
the peers hosting the other ends).

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

## 5. Seams

| Kind | Trait | Implementations | Suite |
|---|---|---|---|
| `log-store` | `LogStore` | `native`, `mem` | `tests/conformance/log-store` |
| `state-store` | `StateStore` | `native`, `mem` | `tests/conformance/state-store` |

Defaults pending research: Raft through `openraft`, `native` stores on `redb`.
Task T-0014 confirms or replaces them.

## 6. Failure behavior

A minority of voters lost: no effect on clients beyond a leader change, complete
within 5 seconds. Quorum lost: all writes fail with `Unavailable`, stale reads
continue. A controller that crashes mid-action must be safe to rerun from the
start.

## 7. Limits

1 MiB per resource. 8 GiB of state per group, with a warning at 6 GiB. 10,000
watches per control node. 64 MiB per write batch.

## 8. Security

Control nodes authenticate each other by node record. Views enforce I-03. All
mutations pass authorization (ARFC 0007) and audit (ARFC 0018) before they are
proposed. Upholds I-11, I-14, I-15.

## 9. Open questions

- Cross-region replication of the global group and its latency budget. Research
  task to be cut in M2.
