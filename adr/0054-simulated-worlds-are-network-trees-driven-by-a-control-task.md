# ADR 0054: Simulated worlds are network trees driven by a control task

- Status: accepted
- Subject: engineering
- Specs: arfc-0005

## Context

T-0009 and T-0010 build and test `alloy-sim` in parallel and need one interface
for worlds, faults, and runs. Scenarios must express NAT layers, partitions, and
crashes at any await point, react to the world's state, and stay a pure function
of the seed. The `mem` stream model had no rule for resets.

## Decision

- A scenario is one future, the control task, run by the seeded scheduler beside
  the host tasks. It builds the world and applies `Fault` values now or at a world
  time, and its result decides the run (ARFC 0005 clauses 3.2.7 and 3.2.8).
- Networks form trees. Each non-root network has one router to its parent.
  Routing is by exact address, then routable child prefix, then upward. A router
  may carry a `Middlebox`, the plug-in `alloy-natlab` implements.
- Link properties belong to a network and apply per crossing. Partitions are
  between sets of hosts, judged by a packet's origin and destination host.
- `mem` streams are reliable segments delayed by the route's latency. An end
  resets on a reset segment, or when a byte it sent is still undelivered 60 s
  after it was due. A segment that reaches an end that can no longer read is
  discarded and answered with at most one reset per direction. A connection
  can be set up only over a route whose delay is below 15 s.
- UDP packets in flight count their size against the sending socket's
  `send_buffer` and a 64 MiB bound per origin host. `send` waits for room, and a
  duplicate that does not fit is not made.
- A scheduled fault that is invalid for the state at its time is skipped with a
  `fault` record. The same fault through `apply` returns an error.

## Consequences

- Double NAT, hairpins, and IPv6 firewalls are each a router with a middlebox.
- Topologies with several paths between two networks need a new ADR.
- Streams ignore loss, jitter, bandwidth, and MTU. Tests of those effects use UDP.
- Latency alone never resets a stream. Partitions, crashes, and dropped or reset
  peer ends do, at the latest 60 s after the first undelivered byte was due.
- A sender into a slow network slows down instead of filling memory, so a world's
  memory for packets is bounded by its host count.

## Alternatives rejected

- Links per host pair: cannot express hosts behind one NAT device.
- A general graph with routing tables: more to build, nothing in M0 to M2 needs it.
- A list of timed events instead of a control task: cannot react to state.
- Packet-level TCP in `mem`: large, and the seam contract does not need it.
- One bound on packets in flight per world: one host's burst would stall every
  other host's `send`.
