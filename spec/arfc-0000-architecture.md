# ARFC 0000: Architecture

- Status: accepted
- Depends on: none
- Crates: all
- Decisions: 0002, 0008, 0009, 0015, 0017, 0019, 0031

## 1. Purpose

Alloy turns any set of Linux machines into one multi-tenant cloud. This ARFC fixes
the overall shape: the processes, the node roles, the layers, and the properties
every other ARFC builds on.

## 2. Model

### 2.1 Hierarchy

`cluster > region > zone > node`. A cluster has one cluster key. A region has one
consensus group for regional state. A zone is a failure domain. A new cluster has
one region, `default`, and one zone, `default`.

### 2.2 Node roles

A node holds any subset of four roles, set at join time and changeable later.

| Role | Does |
|---|---|
| `anchor` | Is reachable by every node. Reflects addresses, relays, admits joins, may hold public addresses |
| `control` | Runs the replicated control plane and serves the API |
| `compute` | Runs guests |
| `storage` | Contributes disks to pools |

The first node holds all four. Roles are independent: losing the only anchor must
not cost the control plane its quorum. A node with no roles is a **client node**:
a person's computer in the mesh, defined in ARFC 0002 section 3.11.

### 2.3 Processes

| Process | One per | Survives `alloyd` restart | Does |
|---|---|---|---|
| `alloyd` | node | n/a | Supervisor, node agent, control plane (on control nodes), API |
| `alloy-net` | node | yes | Mesh dataplane and virtual switch |
| `alloy-blk` | node | yes | Volume I/O for guests |
| `alloy-store` | node with `storage` | yes | Chunk store disk service |
| VMM | guest | yes | One hypervisor process per guest |
| `alloy` | user machine | n/a | CLI |

`alloyd` is the only process that talks to the control plane. The others take
configuration from `alloyd` over a local socket and keep serving their last
configuration when `alloyd` is down. A client node runs `alloyd` and `alloy-net`
only. How each process is installed, and what it may change on its host, is in
ARFC 0017 clauses 3.15 to 3.17.

### 2.4 Layers

```
 surfaces     CLI, console, native API, EC2 API, S3 API, OCI registry
 services     compute, VPC, public addressing, block, object, registry, IAM
 control      replicated state, controllers, scheduler, audit
 storage      pools, placement groups, chunk store, disks
 mesh         AL sessions, path manager, relay, virtual switch
 runtime      alloy-rt: clock, network, disk, randomness, tasks
 underlay     whatever networks and hardware the nodes have
```

A layer uses only the layers below it. The control plane's own traffic rides the
mesh, which is why the mesh bootstraps from the join token alone.

## 3. Behavior

3.1 A single node must be a complete cluster: every service in this document
works on one machine with one disk (I-16).

3.2 A cluster must work with exactly one address reachable by all nodes. That
address may be on a LAN, on another overlay, or on the internet. With one anchor
the CLI and console must show a standing warning, since its loss stops joins,
relayed paths, and public ingress through it.

3.3 Any two nodes must be able to exchange traffic if each can send UDP, or failing
that TCP, to at least one common anchor.

3.4 Every subsystem boundary named in an ARFC's "Seams" section must be a runtime
seam per ARFC 0001.

3.5 Every subsystem must run unmodified on the simulator of ARFC 0005.

3.6 Every resource belongs to exactly one organization and project from creation,
or is explicitly cluster-scoped. There is no single-tenant mode. A fresh cluster
has one organization, `default`.

3.7 All state changes go through the native API of ARFC 0008. The CLI, console,
EC2 surface, and S3 control operations are clients of it.

3.8 Supported hosts: Linux 6.1 or later on x86_64 and aarch64. A node without
hardware virtualization may hold `anchor`, `control`, and `storage`, and may hold
`compute` only under the restriction in ARFC 0011. A client node may also run on
Windows build 17763 or later on x86_64 and aarch64, which takes in Windows 10
version 1809, Windows 11, and Windows Server 2019 and later. The CLI runs on
Linux, macOS, and Windows (ARFC 0019 section 2).

3.9 Alloy coexists with what a host already runs: it changes only the host
objects it owns, and uses no address range that overlaps one in use (ARFC 0017
clause 3.17).

## 6. Failure behavior

| Lost | Still works | Stops |
|---|---|---|
| `alloyd` on a node | Guests, sessions, volume I/O on that node | Changes to that node |
| All anchors | Direct paths, guests, control plane if it has direct paths | Joins, relayed paths, ingress via anchors |
| Control plane quorum | Guests, mesh, volume I/O, reads from stale views | All changes |
| A minority of control nodes | Everything | Nothing |
| A zone | Everything in other zones, given quorum outside it | Guests and unreplicated data in the zone |

## 7. Limits

Design targets, checked by simulation at milestone M10: 10,000 nodes per region,
1,000,000 guests per region, 100,000 VPCs per cluster, 16 regions per cluster.

## 8. Security

See ARFC 0020. This document upholds I-03, I-16, I-17, I-19.
