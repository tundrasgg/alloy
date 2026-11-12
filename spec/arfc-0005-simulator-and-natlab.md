# ARFC 0005: Runtime, simulator, and NAT lab

- Status: accepted
- Depends on: 0001
- Crates: alloy-rt, alloy-sim, alloy-natlab
- Decisions: 0008

## 1. Purpose

All of Alloy runs on a small runtime interface. In production the operating system
implements it. In tests a deterministic simulator does, which lets whole clusters,
hostile networks, and years of faults run in seconds from a seed. This is built
before anything else.

## 2. Model

`alloy-rt` defines five seams:

| Seam | Trait | Gives |
|---|---|---|
| `clock` | `Clock` | Monotonic now, wall now, sleep until, timers |
| `net` | `Net` | Datagram and stream sockets, interface list, batch send and receive |
| `disk-io` | `DiskIo` | Open, read at, write at, sync, truncate, list, direct block access |
| `rng` | `Rng` | Random bytes |
| `spawn` | `Spawn` | Start a task, join it, cancel it |

A `Runtime` value bundles one instance of each. Every component takes a `Runtime`
at construction and uses nothing else to reach the world.

`alloy-sim` implements all five in memory, in one thread, driven by a seeded
scheduler and a virtual clock. A **world** holds hosts, links, routers, and NAT
devices. A **scenario** is a function that builds a world, runs Alloy code on its
hosts, injects faults, and asserts properties.

## 3. Behavior

### 3.1 Runtime

3.1.1 Only implementations of these seams call the operating system for time,
network, storage, randomness, or concurrency. `cargo xtask check-repo` rejects
those calls elsewhere.

3.1.2 The `os` implementation uses `tokio`, batched UDP I/O with segmentation
offload where the kernel offers it, and direct I/O for block devices.

### 3.2 Determinism

3.2.1 A scenario run is a pure function of `(binary, seed)`. Two runs with the
same pair must produce byte-identical event traces (I-20). The trace hash is
printed at the end of every run.

3.2.2 The scheduler picks the next runnable task and the delivery order of
simultaneous events from the seed. Time advances only when no task is runnable.

3.2.3 Virtual time has nanosecond resolution. A scenario covering one virtual hour
of a 100-node cluster must finish in under 60 seconds on one core.

### 3.3 Faults

The world can inject, at any virtual time or at seeded random times: per-link
loss, duplication, reordering, delay, jitter, corruption, and MTU limits with or
without ICMP feedback. One-way and two-way partitions. Host crash at any await
point, with loss of all unsynced disk writes, optionally torn. Host restart. Disk
read errors, corrupt reads, and full disks. Clock skew and steps. CPU stalls.

### 3.4 NAT lab

`alloy-natlab` adds NAT devices to the world, configured along the axes of RFC
4787:

| Axis | Values |
|---|---|
| Mapping | endpoint-independent, address-dependent, address-and-port-dependent |
| Filtering | endpoint-independent, address-dependent, address-and-port-dependent |
| Port assignment | preserving, sequential, random |
| Hairpinning | yes, no |
| Mapping timeout | 30 s to 300 s |
| Layers | single, double (carrier grade in front of a home router) |
| Family | IPv4 only, dual stack, IPv6 only with a stateful firewall |
| UDP | allowed, blocked |

Named presets cover common devices. The **matrix** is every ordered pair of
presets with the expected path class (`direct` or `relayed`) and a time bound. It
lives in `tests/natlab/matrix.toml` and is normative for ARFC 0004.

### 3.5 Real mode

Every NAT preset also has a script that builds it from Linux network namespaces
and nftables. `tests/system` runs the real `alloyd` binaries through a sample of
the matrix. A disagreement between the simulated and real result is a bug in the
model and blocks the milestone.

### 3.6 Checking

`alloy-sim` provides checkers used by scenarios: a linearizability checker for
histories of register and key-value operations, a packet isolation checker (no
packet crosses VNIs), and a durability checker (every acknowledged write is
readable after any allowed failure).

## 5. Seams

The five seams above, each with `os` and `mem`, suites under
`tests/conformance/rt/`.

## 6. Failure behavior

A scenario that fails prints its seed, the trace hash, and the last 200 events. A
scenario that exceeds its virtual time budget fails as a liveness violation.

## 7. Limits

10,000 hosts per world. 2^63 nanoseconds of virtual time. Event traces are
streamed to disk and capped at 1 GiB unless a scenario raises the cap.

## 8. Security

The simulator is test infrastructure and is not built into release binaries.
Upholds I-20 and is the means of testing most other invariants.

## 9. Open questions

- Build on an existing deterministic runtime or write our own. Task T-0003.
- Parameters and real-world frequencies of NAT presets. Task T-0005.
