# ARFC 0004: Path manager and relay

- Status: accepted
- Depends on: 0002, 0003
- Crates: alloy-path, alloy-relay
- Decisions: 0014, 0015, 0016, 0031, 0039, 0045, 0049

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
messages between nodes that have no path yet. A reflection request is a probe
message (ARFC 0003 section 3.6). The anchor sends one answer per accepted
reflection request (section 3.2 step 4), to the request's source endpoint, no
larger than the request, at any time. The answer follows section 3.2 step 5 as if
the anchor were probing the requesting node: it echoes the request's challenge
and carries a challenge of its own when the source is not a verified endpoint of
that node.

3.1.2 Every node keeps a session to at least one anchor, and to two when two
exist, chosen by lowest round trip time. This is the node's **home**. The control
plane publishes each node's homes.

A node sends a reflection request on each home session when the session opens,
when its local candidates change, and whenever it has sent none on that session
for 25 seconds. The observed endpoint of the answer that echoes the node's latest
request is its reflexive candidate. The node answers the anchor's answer when it
carries a challenge (section 3.2 step 5), which verifies the node's endpoint at
the anchor (section 3.2 step 6).

An anchor sends session traffic, relay envelopes, and signaling to a homed node
only at an endpoint of that node it has verified. An anchor **hears** an
endpoint of a homed node when it accepts a transport packet or a relay envelope
of that node from it (ARFC 0003 sections 3.4 and 3.7). Among the verified
endpoints of the node it sends to the one it heard most recently, or, if it has
heard none of them, to the one it verified most recently, except that it keeps
the one it sends to while it has heard that one within the last 1 second. It
keeps at most 4 verified endpoints per homed node; at the limit, a newly verified
one replaces the one it heard least recently. Its verification of an endpoint of
a homed node lasts while the home session is open. An anchor treats a home
session on which it has accepted no packet from the node for 15 seconds (3 home
keepalive periods, section 3.4) as closed.

A node sends session traffic to an anchor, and an anchor to another anchor
(3.1.3), only at an address the control plane publishes for that anchor.

3.1.3 Anchors keep sessions to each other and forward relay envelopes for nodes
homed elsewhere. A relayed packet crosses at most two anchors.

### 3.2 Establishing a path

When node A first has traffic for node B:

1. A sends the traffic through the relay at once. The first packet must not wait
   for path discovery.
2. A fetches B's candidates from its view and signals B, through an anchor, to
   start probing A. B starts a probe set toward A when the signal arrives, within
   the bound of section 3.4. The candidates a node's view holds for a peer are
   that peer's **advertised** candidates.
3. Both sides send probes on every candidate pair whose far end is an advertised
   candidate of the peer, 5 rounds, 200 ms apart, on the same socket that carries
   data. The 5 rounds that one trigger starts (first traffic, the signal of step
   2, or a trigger under section 3.4) form a **probe set**. A node is **probing**
   a peer from the first round of a probe set toward it until 1 second after that
   set's fifth round.
4. Every probe message (ARFC 0003 section 3.6) carries a fresh random 16-byte
   challenge, the echo of a challenge it answers, or both. A **probe** carries a
   challenge and no echo. An **answer** carries an echo. Every probe message has
   the same length on the wire, except a probe padded for path MTU discovery
   (section 3.5). Answers are never padded. A packet is **accepted** when it
   authenticates and passes its replay check: a probe message as ARFC 0003
   section 3.6 defines, a transport packet when it opens and passes the replay
   window of ARFC 0003 section 3.4. Only accepted packets count anywhere in this
   ARFC. A packet that is not accepted, a replay included, changes nothing.
5. A node handles an accepted probe message in this order. First, if it carries
   an echo, the node checks the echo (step 6), which may verify the endpoint the
   message arrived from. Then, if it carries a challenge, the node answers it with
   exactly one answer, sent to the source endpoint the message arrived from,
   whether or not that endpoint is an advertised candidate of the sender, when
   that endpoint is now verified or the node is probing the sender. Otherwise the
   message is dropped silently. The answer echoes the challenge. It carries a
   challenge of its own only when it answers a probe, not an answer, that arrived
   from an endpoint that is not verified (step 6). A message without a challenge
   is never answered.
6. An endpoint E of a peer becomes **verified** when the node accepts a probe
   message from E that echoes a challenge the node sent to E no more than 2
   seconds earlier. A challenge is used up by the first accepted message that
   echoes it, whatever endpoint that message came from. Verification of E lapses
   at the first moment when all of these hold: no `usable` path uses E, the node
   is not probing the peer, and E was last verified 2 seconds or more earlier. A
   verified endpoint that is not an advertised candidate of the peer is a
   **peer-reflexive** candidate of that peer. Peer-reflexive candidates count
   toward the limit of 64 candidates per node (section 7); at the limit, a new one
   replaces the least recently verified peer-reflexive candidate of that peer, or
   is not recorded if there is none. A node never sends a probe without padding
   to a peer-reflexive candidate, in the probe set that verified it or in any
   later one. In every other respect a peer-reflexive candidate is a verified
   endpoint like any other.
7. A node sends probes without padding only to advertised candidates of the peer,
   and padded probes (section 3.5) only to the verified far endpoint of a usable
   direct path. To an endpoint that is neither an advertised candidate of the
   peer nor verified, it sends only answers, at most one per accepted
   challenge-bearing message received from that endpoint, each no larger than
   that message, and the handshake replies of section 8 item 3. It sends
   transport packets (ARFC 0003 section 3.4) and handshake initiations (ARFC 0003
   section 3.1) directly to a peer only at endpoints it has verified.
8. A direct path to an endpoint E of the peer is `usable` at a node while E is
   verified and the node has accepted a transport packet directly from E, not
   inside a relay envelope, within the last 75 seconds (3 keepalive periods,
   section 3.4). That packet may have arrived before or after E was verified. So
   that each side receives one, a node sends a keepalive (an AL packet of kind 0,
   ARFC 0003 section 3.4) to an endpoint of the peer at once when it verifies it,
   and to every verified endpoint of the peer in each later round of its probe
   sets toward that peer. Both conditions are things the node observes itself.
   Because transport packets go directly only to verified endpoints (step 7), a
   transport packet from E shows that the peer has verified an endpoint of this
   node and sends to it from E.
9. A node **hears** an endpoint of the peer when it accepts a transport packet
   directly from it. Among usable paths the active one is chosen by: direct
   before relayed. When no direct path is active, the usable direct path whose far
   endpoint the node heard most recently becomes active. A node keeps its active
   direct path while it is usable, until either of these holds, and then switches
   at once:
   - the node has not heard the active path's far endpoint for 1 second, and has
     heard the far endpoint of another usable direct path since; the path whose
     far endpoint it heard most recently becomes active;
   - another usable direct path whose far endpoint the node heard within the last
     1 second has a smoothed round trip time at least 20% lower; that path
     becomes active.

A node must be reachable through a relay within 2 seconds of joining, or within 5
seconds when no anchor answers it over UDP and it reaches them over the `stream`
underlay (section 3.6). A direct path, where section 3.3 expects one, must become
active within 5 seconds of first traffic. That bound assumes that the signal of
step 2 reaches B within 750 ms of A's first traffic and that the one-way delay
between A and B is at most 500 ms. Within these delays the two first probe sets
suffice: in cases 2 and 3 of section 3.3 the last endpoint a side needs becomes
usable four one-way delays after B's first probe, at most 2.75 seconds after
first traffic, and the 2 second floor of step 6 keeps every verification that
long. A later signal can reach B after A's last probe round has passed, and the
two probe sets then do not overlap enough to verify the endpoints that cases 2
and 3 need. Beyond these delays the bound does not apply, and the pair may stay
on the relay.

### 3.3 NAT traversal

Simultaneous probing with answers at the source of each probe (section 3.2)
decides the path class. A pair is `direct` when each node holds a usable direct
path to the other (section 3.2 step 8), and `relayed` when neither does. Mapping
and filtering are the behaviors of RFC 4787 as ARFC 0005 section 3.4 models them;
address-dependent and address-and-port-dependent mapping are both called
endpoint-dependent here.

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
endpoint-independent mapping. A side behind two layered devices counts as one
device, and the rule above applies to that device:

- Its mapping is the more dependent mapping of the two layers.
- Its filtering is the stricter of two values, in the order endpoint-independent,
  address-dependent, address-and-port-dependent: the outer layer's filtering, and
  the inner layer's filtering. The inner layer's filtering is taken as
  endpoint-independent when the inner layer has endpoint-independent mapping and
  address-dependent filtering and the outer layer has endpoint-dependent mapping.
  In every other case each layer's filtering is taken as it is configured.

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
- A direct path on which the node has accepted no transport packet from its far
  endpoint for 3 keepalive periods (75 seconds) is no longer usable (section 3.2
  step 8) and is `failed`. Traffic moves at once to the path section 3.2 step 9
  chooses, the relay when no direct path is usable.
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
  minutes. A change of either side's candidates starts a probe set at once,
  whether the session is on a relay or on a direct path.
- A node sends session traffic for a peer only on its active path: to the
  verified far endpoint of a usable direct path, or through the relay. Session
  traffic is the session's transport packets, the keepalives of this section
  included, and its handshake messages (ARFC 0003 sections 3.1 and 3.4). Two
  kinds of packet go elsewhere: the keepalives of section 3.2 step 8, to every
  verified endpoint of the peer, and a handshake response or cookie reply, to
  the source of the initiation it answers (section 8 item 3).
- An accepted packet from an endpoint of the peer that is not verified never
  changes where the node sends. When it is a transport packet received directly,
  not inside a relay envelope, and the node is not probing that peer, it starts a
  probe set toward the peer. After a NAT rebinding, the path to the peer's new
  endpoint becomes active by section 3.2 step 9 once it is usable and the old
  endpoint has not been heard for 1 second.
- Every probe set started under this section signals the peer as in section 3.2
  step 2, so both sides run a probe set at once. A probe set that a node starts
  when a signal from the peer arrives is not signaled back.
- A node starts at most 2 probe sets toward one peer in any 5 seconds and at most
  20 in any 5 minutes, whatever triggers them: first traffic, a signal from the
  peer, a repeat, a change of candidates, or a packet from an endpoint that is not
  verified. A trigger beyond the bound starts no probe set when it arrives. The
  node starts one probe set, signaled to the peer, at the earliest time the bound
  allows, and every trigger that arrives before then is served by that one.

### 3.5 Path MTU

Each direct path's MTU is discovered by padded probes (the method of RFC 8899):
start at 1,280, search up to the interface MTU, re-probe every 10 minutes and on
loss of large packets only. Padded probes go only to the verified far endpoint of
a usable direct path (section 3.2 step 7). Relayed paths use 1,280 until probed.
The payload budget is the path MTU minus the overhead in ARFC 0003.

### 3.6 Underlays

`udp` is the default. `stream` (TLS 1.3 over TCP port 443 to an anchor) is used
only for the home session and relayed traffic, and only after 3 seconds without a
UDP reply from any anchor, counted from the node's first UDP packet to an anchor.
Such a node must then be reachable through a relay within 5 seconds of joining
(section 3.2). A node on `stream` retries UDP every 5 minutes. On `stream` the
connection the node opened is the path: the anchor sends to the node only on that
connection, and section 3.2 step 6 does not apply to it.

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

64 candidates per node. 2 probe sets per peer in any 5 seconds and 20 in any 5
minutes, with later triggers deferred (section 3.4). 64 outstanding
challenges per peer sent in answers (section 3.2 step 5), oldest dropped first.
4 verified endpoints per homed node at an anchor, least recently heard replaced
first (section 3.1.2). 4,096 active sessions per node by default, least recently
used closed first. Relay forwarding per anchor is bounded by a configured rate,
default unlimited.

## 8. Security

Probes and signaling are authenticated. Probe messages and transport packets are
replay protected (ARFC 0003 sections 3.4 and 3.6), and only accepted packets
count in this ARFC (section 3.2 step 4): a replayed packet changes nothing.

A node sends to an anchor at an address the control plane publishes for it
(section 3.1.2), and on a `stream` connection it opened (section 3.6), the
packets ARFC 0003 and this ARFC call for. Toward any other endpoint it has not
verified (section 3.2 step 6), a node or an anchor sends only these packets, and
no others:

1. Probes without padding, only if the endpoint is an advertised candidate of the
   peer: one per candidate pair per round, 5 rounds per probe set, and at most 2
   probe sets toward that peer in any 5 seconds and 20 in any 5 minutes (section
   3.4).
2. Answers: at most one per accepted challenge-bearing message received from that
   endpoint, each no larger than that message, and, at a node, only while it is
   probing the sender (section 3.2 steps 5 and 7). An anchor sends at most one
   answer per accepted reflection request, no larger than the request, at any
   time (section 3.1.1).
3. Handshake responses and cookie replies (ARFC 0003 section 3.1): at most one per
   initiation received from that endpoint, each smaller than the initiation.

Everything else, transport packets with their keepalives, handshake initiations,
relay envelopes, signaling, and padded probes, goes only to verified endpoints
(section 3.2 steps 7 and 8, section 3.1.2), to anchors at their published
addresses, and on a `stream` connection the node opened. An accepted packet from
an endpoint that is not verified never changes where a node or an anchor sends
(sections 3.1.2 and 3.4). At most it starts a probe set toward the peer's
advertised candidates, within the bound of item 1. So a probe message that
reaches a node from a spoofed source makes it send at most one packet, of at most
the message's size, to that source, only while it is probing the message's
sender; an anchor sends at most one such packet. A transport packet from a
spoofed source makes a node send nothing to that source.

Traffic on a direct path whose far endpoint stops sending ends when the path is
`failed`, at most 75 seconds after the last transport packet accepted from it
(section 3.4). An anchor's traffic to a homed node's endpoint ends when it
selects another endpoint of that node, or at most 15 seconds after it last
accepted a packet from the node, when it treats the home session as closed
(section 3.1.2).

A candidate is never trusted until a probe round trip succeeds. A peer,
compromised or not, can direct packets at a third party only by advertising its
address as a candidate, and then only as item 1 allows: from each node it has a
session with, at most 5 probe rounds per probe set and 20 probe sets in any 5
minutes. Upholds I-06, I-08.

### Redirection by an underlay attacker

Verification (section 3.2 step 6) shows that a challenge reached the peer and
that an echo of it came back from the endpoint. No field of a probe message
binds the endpoint (ARFC 0003 section 3.6), and no node can observe the endpoint
its peer sends to. An underlay attacker (ARFC 0020 section 2.1) that reads a
node's packets and injects copies with any source V can therefore complete the
verification of V. It can do so only by getting copies accepted from V in place
of the genuine packets: copies that overtake them, after which the replay check
rejects the genuine ones, or copies of packets the network dropped. Copies that
arrive after the genuine packets are not accepted and change nothing. With V
verified and transport packets accepted from V, the attacker can make a node use
V as its active path to a peer (section 3.2 step 9), or an anchor send to V as a
homed node's endpoint (section 3.1.2). What V then receives is bounded:

- Kind and rate: only packets the node would send to that peer on a direct path,
  or the anchor to that homed node: session traffic, keepalives, relay
  envelopes, signaling, and padded probes. The attacker cannot raise their rate.
- Start: a node or an anchor moves to V only once it has not heard the endpoint
  it sends to for 1 second, or, at a node, by the round trip time rule of section
  3.2 step 9, which needs a transport packet from V accepted within the last
  second. While the peer or homed node sends steadily, the attacker must block or
  overtake every packet from it for 1 second, as ARFC 0020 section 3.2 allows
  it. While it is idle, its packets arrive up to 25 seconds apart at a node and
  5 seconds apart at an anchor, and overtaking one suffices.
- End at a node: the node sends session traffic to V until 1 second after the
  last transport packet the attacker gets accepted from V, or until it next hears
  the peer on another usable direct path, whichever is later, and in no case
  more than 75 seconds after that last packet (section 3.4). Until the path to V
  fails, at most 75 seconds after that last packet, V may also receive the
  keepalives of section 3.2 step 8 and padded probes, as the far endpoint of any
  usable path does.
- End at an anchor: the anchor sends to V until 1 second after the last transport
  packet or relay envelope the attacker gets accepted from V, or until it next
  hears the node at another verified endpoint, whichever is later. The node sends
  on its home session at least every 5 seconds (section 3.4), so that is at most
  5 seconds after that last packet. If V displaced the node's endpoint from the 4
  the anchor keeps (section 3.1.2), the anchor sends to V until the node's next
  reflection request, at most 25 seconds later, is answered and echoed.

Each bound runs from the last packet the attacker gets accepted from V. An
attacker that keeps getting such packets accepted keeps the redirection going
for as long as it does so.

The same attacker can make a node take V as its reflexive candidate (section
3.1.2), by getting a reflection request accepted from V in place of the node's
own, until the node's next reflection request is answered, at most 25 seconds
later. Peers then probe V only as item 1 allows, the bound above for a candidate
a peer advertises.

## 9. Open questions

- Port prediction for endpoint-dependent NAT pairs. Research task to be cut in M1.
