# ARFC 0004: Path manager and relay

- Status: accepted
- Depends on: 0002, 0003
- Crates: alloy-path, alloy-relay
- Decisions: 0014, 0015, 0016, 0031

## 1. Purpose

Finds and keeps the best way to carry AL packets between two nodes: a direct path
when the network allows one, a relayed path otherwise, and a move between them
without the session noticing.

## 2. Model

An **endpoint** is `(underlay kind, address, port)`. A node's **candidates** are:

| Kind | Source |
|---|---|
| local | Addresses on its interfaces |
| reflexive | Its address as an anchor sees it |
| mapped | A port mapping it obtained from its gateway (`portmap` seam) |
| relay | An anchor it is homed to |

A **path** is a pair of endpoints, or a relay. Path states: `unknown`, `probing`,
`usable`, `failed`. Each peer pair has at most one **active** path at a time.

## 3. Behavior

### 3.1 Anchors

3.1.1 An anchor answers reflection requests (a probe to which it replies with the
source endpoint it observed), forwards relay envelopes, and passes signaling
messages between nodes that have no path yet.

3.1.2 Every node keeps a session to at least one anchor, and to two when two
exist, chosen by lowest round trip time. This is the node's **home**. The control
plane publishes each node's homes.

3.1.3 Anchors keep sessions to each other and forward relay envelopes for nodes
homed elsewhere. A relayed packet crosses at most two anchors.

### 3.2 Establishing a path

When node A first has traffic for node B:

1. A sends the traffic through the relay at once. The first packet must not wait
   for path discovery.
2. A fetches B's candidates from its view and signals B, through an anchor, to
   start probing A.
3. Both sides send probes to every candidate pair, 5 rounds, 200 ms apart, on the
   same socket that carries data.
4. The first candidate pair to complete a probe round trip in both directions
   becomes `usable`.
5. Among usable paths the active one is chosen by: direct before relayed, then
   lowest smoothed round trip time, with a 20% hysteresis before switching between
   direct paths.

A node must be reachable through a relay within 2 seconds of joining, and a direct
path, where the NAT types allow one, must become active within 5 seconds of first
traffic.

### 3.3 NAT traversal

Simultaneous probing opens endpoint-independent NATs. When both sides are behind
endpoint-dependent NATs, version 1 stays on the relay. The expected result for
every pairing of NAT types is the matrix in `tests/natlab`, which is normative.

### 3.4 Keeping a path

- An idle direct path sends a keepalive every 25 seconds.
- A direct path with no authenticated packet for 3 keepalive periods is `failed`,
  and traffic moves to the relay at once.
- While on a relay, direct probing repeats with backoff from 5 seconds to 5
  minutes, and immediately when either side's candidates change.
- An authenticated packet from a new source endpoint moves the session's return
  path to that endpoint at once.

### 3.5 Path MTU

Each direct path's MTU is discovered by padded probes (the method of RFC 8899):
start at 1,280, search up to the interface MTU, re-probe every 10 minutes and on
loss of large packets only. Relayed paths use 1,280 until probed. The payload
budget is the path MTU minus the overhead in ARFC 0003.

### 3.6 Underlays

`udp` is the default. `stream` (TLS 1.3 over TCP port 443 to an anchor) is used
only for the home session and relayed traffic, and only after 3 seconds without
any UDP reply from every anchor. A node on `stream` retries UDP every 5 minutes.

### 3.7 Relay-only nodes

A node whose only usable paths are relayed is marked `relay-only` in its status.
It may hold any role. The CLI and console must show the mark and the relay's
measured throughput. A cluster setting, default 100 Mbit/s per node, caps the
relayed traffic of each relay-only node.

### 3.8 Lazy sessions

Nodes open sessions only to peers they have traffic for, and close a session idle
for 5 minutes, except home sessions. No node keeps state for every other node.

## 5. Seams

| Kind | Trait | Implementations | Suite |
|---|---|---|---|
| `underlay` | `Underlay` | `udp`, `stream`, `mem` | `tests/conformance/underlay` |
| `portmap` | `PortMapper` | `none`, `pcp`, `mem` | `tests/conformance/portmap` |

## 6. Failure behavior

Loss of a home anchor: the node re-homes to the next anchor within 5 seconds.
Established direct paths are unaffected (I-08). With no anchor reachable, a node
keeps its direct paths and retries anchors with backoff from 1 to 60 seconds.

## 7. Limits

64 candidates per node. 8 concurrent probe sets per peer. 4,096 active sessions
per node by default, least recently used closed first. Relay forwarding per
anchor is bounded by a configured rate, default unlimited.

## 8. Security

Probes and signaling are authenticated. A candidate is never trusted until a probe
round trip succeeds, so a peer cannot direct traffic at a third party beyond 5
probe rounds. Upholds I-06, I-08.

## 9. Open questions

- Port prediction for endpoint-dependent NAT pairs. Research task to be cut in M1.
