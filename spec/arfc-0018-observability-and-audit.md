# ARFC 0018: Observability and audit

- Status: accepted
- Depends on: 0002, 0006, 0007, 0017
- Crates: alloy-obs, alloy-audit
- Decisions: 0028, 0064, 0065

## 1. Purpose

Lets cloud admins see the health of the cluster, lets tenants see their own
resources, and keeps a record of every change that cannot be quietly altered.

## 2. Model

| Signal | Scope | Kept |
|---|---|---|
| metrics | cluster, and per resource for tenants | 15 days at full resolution, 13 months downsampled |
| logs | per process, structured | 7 days on the node |
| traces | per request, sampled | 3 days |
| events | per resource: state changes and reasons | 30 days |
| audit records | per mutating call and per authentication | 13 months, configurable up |

**First deployment.** M11 rung 2 builds clauses 3.3.1 to 3.3.3, 3.3.4 without
signed checkpoints and `alloy audit verify`, 3.3.6, and 3.3.7. The audit records
that ARFC 0002 and ARFC 0007 require on the paths rung 2 uses, such as join
refusals and login code exchanges, are among them. Everything else here is built
by later tasks, the record encoding included: until the task that builds `alloy
audit verify` fixes it, records written by preview releases (ARFC 0017 clause
3.13) need not verify.

## 3. Behavior

### 3.1 Metrics, logs, traces

3.1.1 Every process exposes metrics in the Prometheus text format on a local
socket. `alloyd` aggregates them and serves cluster metrics to cloud admins and
per-resource metrics to tenants through the API.

3.1.2 Metric names are `alloy_<subsystem>_<name>_<unit>`. Labels never contain
tenant-chosen strings except resource uids.

3.1.3 Logs are JSON lines with `time`, `level`, `target`, `message`, and fields.
Traces follow OpenTelemetry, and a trace id from an API call is carried through
every internal call it causes.

3.1.4 Metrics, logs, and traces can be exported over OTLP.

3.1.5 A tenant must never see a metric, log, event, or trace of another org
(I-02).

### 3.2 Events

Every status change of a resource emits an event on that resource with a reason
code and a message. A resource that fails says why in its status and in an event.

### 3.3 Audit

3.3.1 An audit record is written for every mutating API call, every
authentication attempt, every authorization denial, and every cloud admin action,
including break glass access (ARFC 0007). A heartbeat (ARFC 0006 clause 3.13)
is a node message, not an API call, and the AL handshake of the session it comes
on, not the heartbeat, authenticates the node. A heartbeat writes a record only
for a change it commits, one per change as clause 3.3.7 lists, so a heartbeat
that changes only soft state (ARFC 0006 section 2.3), or that the control plane
drops or refuses, writes none. The node lifecycle controller's commits of
`unreachable_since` (ARFC 0006 clause 3.16) write none either.

3.3.2 A record holds: sequence number, time, principal, org, source address,
action, resource, request digest, outcome, and the hash of the previous record.
It never holds secrets or request bodies.

3.3.3 The record is committed in the same control plane transaction as the change
it describes. A change must not commit without its record (I-15).

3.3.4 Records form a hash chain per org and one for the cluster. Since a record
commits with its change (3.3.3) and each consensus group has its own log (ARFC
0006 section 2.2, ADR 0064), each group keeps its own chains, of the records it
commits, in its log order. Each record's hash covers its content and the
previous hash. The first record of a chain takes 32 zero bytes as the previous
hash. A signed checkpoint of each chain
head is written every 5 minutes. `alloy audit verify` recomputes a chain and
reports the first break.

3.3.5 Org admins read their org's records. Cloud admins read the cluster chain.
Records can be streamed to an external system through the `audit-sink` seam.

3.3.6 Records are never updated or deleted before their retention ends, by any
role.

3.3.7 Nodes. Each event below writes one record in the global group's cluster
chain, with no org, in the transaction of the change it describes, or in a
transaction of its own when it changes nothing. The outcome is `ok`, `pending`,
or the error code or refusal the caller got.

| Action | Written for | Principal |
|---|---|---|
| `cluster:Init` | `alloy init` committing the cluster (ARFC 0002 section 3.1). It is the first record of the global group's cluster chain, and `init` writes a second one, with the same fields, as the first record of the regional group's cluster chain, in the transaction that creates what `init` puts there. The source address of both is the node's infrastructure address (ARFC 0002 section 3.4) | `root@<node name>` of the new node |
| `cluster:JoinNode` | Every join request that reaches ARFC 0002 section 3.3 step 6 and is admitted, left `pending`, or refused, and every `version` refusal of step 5. A retry that creates nothing writes none | `token:<token_id>` as the request names it, `anonymous` for `version` |
| `cluster:ApproveNode`, `cluster:RejectNode`, `cluster:UpdateNode`, `cluster:DeleteNode` | Each call, whatever its outcome. `DeleteNode` covers `alloy node leave` (ARFC 0002 section 3.9) | The caller |
| `cluster:RenewNode` | Each record renewal and each `node_unknown` refusal (ARFC 0002 section 3.5) | `node:<node name>`, `anonymous` for a refusal |
| `cluster:SetNodeRelease` | Each running release write (ARFC 0017 clause 3.11), applied or refused, a node's heartbeat that carries one included (ARFC 0006 clause 3.14) | `node:<node name>` |
| `cluster:ReportNode` | Only the heartbeat that sets `reported` (ARFC 0006 clause 3.14) | `node:<node name>` |
| `cluster:ExpireNode` | The deletion of a `joining` node or a `pending` request by the node lifecycle controller (ARFC 0006 clause 3.16) | `system:node-lifecycle` |

The resource is the node's name. The source address is the address the call
came from, as the control plane saw it, never an address that another node
states. For a node message (ARFC 0002 section 3.13) it is:

- for one that an anchor holding `control` takes on a session from an
  unregistered initiator, the underlay source address of the packet that
  carried it to that anchor;
- for one that arrives in a `forwarded`, the infrastructure address of the
  anchor that forwarded it, and never the address the `forwarded` names;
- for every other node message, which arrives on an AL session with its sender
  over a direct or a relayed path, the sender's infrastructure address, which
  is the source address of the IPv6 packet that carries it;
- for a control node's own requests and heartbeats, which make no node message,
  that node's infrastructure address.

The request digest is BLAKE3 over the request's Protocol Buffers encoding, the `NodeStatus` for a heartbeat, with every secret field,
such as the login code's `secret` in `ExchangeLoginCode`, emptied first, and 32 zero bytes for
`cluster:Init` and `cluster:ExpireNode`.

### 3.4 Health

`alloy doctor` checks a node or the cluster and prints findings with the command
that fixes each: clock, connectivity to anchors, path classes, control plane
health, disk and pool health, certificate and record expiry, version skew.

## 5. Seams

| Kind | Trait | Implementations | Suite |
|---|---|---|---|
| `metrics-sink` | `MetricsSink` | `native`, `otlp`, `mem` | `tests/conformance/metrics-sink` |
| `audit-sink` | `AuditSink` | `native`, `otlp`, `mem` | `tests/conformance/audit-sink` |

## 6. Failure behavior

An unreachable external sink buffers up to its limit, then drops metrics and
traces with a counter. Audit records are never dropped: the native chain is the
record, and external sinks resume from their last acknowledged sequence number.

## 7. Limits

1 million active metric series per region. 64 KiB per log line. 4 KiB per audit
record. 1 GiB of export buffer per sink.

## 8. Security

Upholds I-02, I-15, I-22.
