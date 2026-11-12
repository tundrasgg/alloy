# ARFC 0018: Observability and audit

- Status: accepted
- Depends on: 0006, 0007
- Crates: alloy-obs, alloy-audit
- Decisions: 0028

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
including break glass access (ARFC 0007).

3.3.2 A record holds: sequence number, time, principal, org, source address,
action, resource, request digest, outcome, and the hash of the previous record.
It never holds secrets or request bodies.

3.3.3 The record is committed in the same control plane transaction as the change
it describes. A change must not commit without its record (I-15).

3.3.4 Records form a hash chain per org and one for the cluster. Each record's
hash covers its content and the previous hash. A signed checkpoint of each chain
head is written every 5 minutes. `alloy audit verify` recomputes a chain and
reports the first break.

3.3.5 Org admins read their org's records. Cloud admins read the cluster chain.
Records can be streamed to an external system through the `audit-sink` seam.

3.3.6 Records are never updated or deleted before their retention ends, by any
role.

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
