# ARFC 0004: Path manager and relay

- Status: accepted
- Depends on: 0002, 0003
- Crates: alloy-path, alloy-relay
- Decisions: 0014, 0015, 0016, 0031, 0039, 0045

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
messages between nodes that have no path yet. It sends one answer per reflection
request, to the request's source endpoint, no larger than the request.

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
   start probing A. B starts a probe set toward A when the signal arrives.
3. Both sides send probes to every candidate pair, 5 rounds, 200 ms apart, on the
   same socket that carries data. The 5 rounds that one trigger starts (first
   traffic, the signal of step 2, or a repeat under section 3.4) form a **probe
   set**. A node is **probing** a peer from the first round of a probe set toward
   it until 1 second after that set's fifth round.
4. Every probe message (ARFC 0003 section 3.6) carries a fresh random 16-byte
   challenge, the echo of a challenge it answers, or both. A **probe** carries a
   challenge and no echo. An **answer** carries an echo. Every probe message has
   the same length on the wire, except a probe padded for path MTU discovery
   (section 3.5). Answers are never padded.
5. A node answers each authenticated probe message that carries a challenge with
   exactly one answer, sent to the source endpoint the message arrived from,
   whether or not that endpoint is a known candidate of the sender. The answer
   echoes the challenge. It carries a challenge of its own only when it answers a
   probe, not an answer, that arrived from an endpoint that is not verified (step
   6). A message without a challenge is never answered. A message from an
   endpoint that is not verified is answered only while the node is probing the
   sender, and is otherwise dropped silently.
6. An endpoint of a peer becomes **verified** when an authenticated message from
   that endpoint echoes a challenge the node sent to that endpoint. A challenge is
   accepted once, and only within 2 seconds of being sent. Verification lapses
   when no `usable` path uses the endpoint and the node is not probing the peer. A
   verified endpoint that is not a known candidate of the peer is a
   **peer-reflexive** candidate of that peer. Peer-reflexive candidates count
   toward the limit of 64 candidates per node (section 7); at the limit, a new one
   replaces the least recently verified peer-reflexive candidate of that peer, or
   is not recorded if there is none.
7. A node sends probes only to known candidates of the peer and to verified
   endpoints. To any other endpoint it sends only answers: at most one per
   challenge-bearing message received from that endpoint, each no larger than
   that message.
8. A candidate pair completes a round trip when the endpoint at its far end
   becomes verified through a challenge sent on that pair. The first candidate
   pair to complete a round trip in both directions becomes `usable`.
9. Among usable paths the active one is chosen by: direct before relayed, then
   lowest smoothed round trip time, with a 20% hysteresis before switching between
   direct paths.

A node must be reachable through a relay within 2 seconds of joining, or within 5
seconds when no anchor answers it over UDP and it reaches them over the `stream`
underlay (section 3.6). A direct path, where section 3.3 expects one, must become
active within 5 seconds of first traffic.

### 3.3 NAT traversal

Simultaneous probing with answers at the source of each probe (section 3.2)
decides the path class. Mapping and filtering are the behaviors of RFC 4787 as
ARFC 0005 section 3.4 models them; address-dependent and
address-and-port-dependent mapping are both called endpoint-dependent here.

For two nodes behind different NAT devices, each device with one external address
per IP family, a direct path is expected when the two have a common IP family on
which both can send UDP and one of these holds:

1. both sides have endpoint-independent mapping;
2. one side has endpoint-independent mapping and endpoint-independent or
   address-dependent filtering, whatever the other side's behavior;
3. both sides have address-dependent mapping and endpoint-independent filtering.

Every other combination of the mapping and filtering values of ARFC 0005 section
3.4 stays on the relay in version 1. Opening those needs port prediction
(section 9).

A side with no address translation (a stateful firewall only) has
endpoint-independent mapping. A side behind two layered devices has the most
dependent mapping of its two layers and the strictest filtering of its two
layers, in the order endpoint-independent, address-dependent,
address-and-port-dependent, with one exception: an inner layer with
endpoint-independent mapping and address-dependent filtering, behind an outer
layer with endpoint-dependent mapping, counts as endpoint-independent filtering.

This rule covers nodes behind different devices. For pairs on one local network
and pairs behind one device, the class is the one section 3.2 produces in the lab
model of ARFC 0005 section 3.4.

The expected path class and time bound for every ordered pair of lab presets,
including pairs on one local network and pairs behind one device, is the matrix in
`tests/natlab` (ARFC 0005 section 3.4). The matrix is normative and agrees with
this section. The time bounds are those of section 3.2.

### 3.4 Keeping a path

- Each side of a direct path sends a keepalive (an AL packet of kind 0, ARFC 0003
  section 3.4) on it whenever it has sent nothing on that path for 25 seconds. A
  NAT that refreshes its state on outbound packets, as RFC 4787 REQ-6 requires
  (`research/R-0003.md`), and whose mapping timeout is 30 seconds or more
  therefore never drops the state of an active direct path.
- A direct path with no authenticated packet for 3 keepalive periods is `failed`,
  and traffic moves to the relay at once.
- Version 1 does not adapt the keepalive interval to the NAT. Behind a NAT whose
  mapping timeout is under 25 seconds, packets sent on an idle direct path may be
  lost until the path carries packets again or the rule above marks it `failed`.
  The lab exercises this case with the preset in ARFC 0005 section 3.4.
- Each node sends a keepalive on each home session (section 3.1.2) whenever it
  has sent nothing on that session for 5 seconds. Behind a NAT that refreshes its
  state on outbound packets and whose mapping timeout is 10 seconds or more, the
  relay route to the node and its reflexive candidate therefore stay open however
  long the node is otherwise idle.
- While on a relay, direct probing repeats with backoff from 5 seconds to 5
  minutes, and immediately when either side's candidates change. Each repeat
  signals the peer as in section 3.2 step 2, so both sides run a probe set at
  once.
- An authenticated packet from a new source endpoint moves the session's return
  path to that endpoint at once.

### 3.5 Path MTU

Each direct path's MTU is discovered by padded probes (the method of RFC 8899):
start at 1,280, search up to the interface MTU, re-probe every 10 minutes and on
loss of large packets only. Relayed paths use 1,280 until probed. The payload
budget is the path MTU minus the overhead in ARFC 0003.

### 3.6 Underlays

`udp` is the default. `stream` (TLS 1.3 over TCP port 443 to an anchor) is used
only for the home session and relayed traffic, and only after 3 seconds without a
UDP reply from any anchor, counted from the node's first UDP packet to an anchor.
Such a node must then be reachable through a relay within 5 seconds of joining
(section 3.2). A node on `stream` retries UDP every 5 minutes.

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

64 candidates per node. 8 concurrent probe sets per peer. 64 outstanding
challenges per peer sent in answers (section 3.2 step 5), oldest dropped first.
4,096 active sessions per node by default, least recently used closed first.
Relay forwarding per anchor is bounded by a configured rate, default unlimited.

## 8. Security

Probes and signaling are authenticated.

No amplification toward an unverified source: toward an endpoint that is neither
a known candidate of the peer nor verified, a node sends only answers, at most one
packet per authenticated challenge-bearing message received from that endpoint,
each no larger than that message, and only while it is probing that peer (section
3.2 steps 5 and 7). An anchor sends at most one answer per reflection request, no
larger than the request (section 3.1.1). A replayed or spoofed probe therefore
makes a node send at most one packet, of at most the probe's size, to the spoofed
source while it is probing the probe's sender, and nothing at other times; it
makes an anchor send at most one such packet. Probes carry no counter (ARFC 0003
section 3.6), so this bound, not a replay check, is what limits a replayed probe.

A candidate is never trusted until a probe round trip succeeds, so a peer cannot
direct traffic at a third party beyond 5 probe rounds per probe set to the
candidates it advertises. Upholds I-06, I-08.

## 9. Open questions

- Port prediction for endpoint-dependent NAT pairs. Research task to be cut in M1.
