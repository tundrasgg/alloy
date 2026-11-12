# ARFC 0001: Runtime seams

- Status: accepted
- Depends on: 0000
- Crates: alloy-seam, alloy-rt
- Decisions: 0009

## 1. Purpose

Every subsystem boundary in Alloy is replaceable at runtime, from the first
commit. A seam lets a cluster pick, mix, and migrate between implementations per
node or per resource without rebuilding, and lets the simulator stand in for the
world.

## 2. Model

A **seam** is four things:

1. an object-safe Rust trait,
2. a registry kind, a short name such as `disk` or `hypervisor`,
3. a configuration schema for each implementation,
4. a conformance suite generic over the trait.

An **implementation** registers under `(kind, name)`. An **instance** is an
implementation built from configuration at runtime. Instances are chosen by
control plane state, for example a pool names its storage backend and a node names
its hypervisor.

## 3. Behavior

3.1 `alloy-seam` provides the registry: `register(kind, name, factory)` at process
start, and `instantiate(kind, name, config) -> Arc<dyn Trait>` at any time.

3.2 Implementation choice must come from configuration or control plane state at
runtime. Cargo features must not select implementations. They may only exclude
code that cannot compile on a platform.

3.3 Several instances of one seam, of the same or different implementations, must
be able to run in one process at once.

3.4 Every seam must have an in-memory implementation, registered as `mem`, that
runs on the simulator. It is a real implementation and passes the same suite.

3.5 An implementation must not be registered in a release build until it passes
the seam's conformance suite in CI.

3.6 Seam traits on a hot path must be batch oriented: methods take and return
slices or vectors of work, so dispatch cost is per batch.

3.7 Seam traits must not expose implementation types. Configuration crosses the
seam as a typed, versioned, serializable value.

3.8 Each seam declares a swap class:

| Class | Meaning |
|---|---|
| `live` | An instance can be replaced under load by the swap protocol below |
| `drain` | A resource is moved from one instance to another online, then the old one is retired |
| `restart` | Changing it restarts one process. Guests keep running (I-17) |

3.9 Live swap protocol: `prepare(new)` builds and validates the new instance,
`quiesce(old)` stops intake and flushes, `transfer` moves state through the seam's
own export and import methods, `activate(new)`, `retire(old)`. Any step may fail
before `activate`, and the old instance then resumes. After `activate` there is no
automatic way back.

## 5. Seams

The initial set. Each owning ARFC defines the trait.

| Kind | Owner | Swap class | First implementations |
|---|---|---|---|
| `clock`, `net`, `disk-io`, `rng`, `spawn` | 0005 | restart | `os`, `mem` (simulator) |
| `underlay` | 0004 | live | `udp`, `stream`, `mem` |
| `portmap` | 0004 | live | `none`, `pcp`, `mem` |
| `log-store`, `state-store` | 0006 | restart | `native`, `mem` |
| `authn` | 0007 | live | `local`, `oidc`, `mem` |
| `key-provider` | 0007 | live | `cluster`, `mem` |
| `attachment` | 0010 | drain | `anchor`, `l2`, `routed`, `bgp`, `mem` |
| `hypervisor` | 0011 | drain | `cloud-hypervisor`, `mem` |
| `sandbox` | 0011 | drain | `namespace`, `mem` |
| `scheduler-policy` | 0011 | live | `default`, `mem` |
| `disk` | 0012 | drain | `extent`, `file`, `mem` |
| `chunk-store` | 0012 | drain | `native`, `mem` |
| `block-backend` | 0013 | drain | `native`, `mem` |
| `blob-backend` | 0014 | drain | `native`, `mem` |
| `index-store` | 0014 | drain | `native`, `mem` |
| `metrics-sink`, `audit-sink` | 0018 | live | `native`, `otlp`, `mem` |

Adding a seam or an implementation is a spec change to the owning ARFC and to this
table.

## 6. Failure behavior

An instance that fails to build from its configuration must leave the previous
instance serving and must surface the error on the resource that asked for it. A
resource bound to an implementation the node does not have must report
`SeamUnavailable` and must not fall back to a different implementation.

## 7. Limits

Registry names are 1 to 32 characters, `[a-z0-9-]`. Configuration values are at
most 64 KiB serialized.

## 8. Security

Implementations are compiled in. Loading code at runtime from outside the binary
is not supported. Upholds I-19.
