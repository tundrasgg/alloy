# ARFC 0004: Path manager and relay

- Status: accepted
- Depends on: 0002, 0003
- Crates: alloy-path, alloy-relay
- Decisions: 0014, 0015, 0016, 0031, 0039, 0045, 0061

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
larger than the request, at any time. The answer echoes the request's challenge
and always carries a fresh challenge of its own.

3.1.2 Every node keeps a session to at least one anchor, and to two when two
exist, chosen by lowest round trip time. This is the node's **home**. The control
plane publishes each node's homes.

A node sends a reflection request on each home session when the session opens,
when its local candidates change, and whenever it has sent none on that session
for 25 seconds. The observed endpoint of the answer that echoes the node's latest
request is its reflexive candidate. The node answers the challenge that answer
carries with exactly one answer, sent to the published address the request went
to.

An anchor sends session traffic, relay envelopes, and signaling to a node homed
to it over UDP only at that node's **current endpoint** (a node homed over
`stream` follows section 3.6 instead): the endpoint from which the node most
recently echoed a challenge that the anchor sent to that same endpoint no more
than 2 seconds earlier, in an accepted probe message (section 3.2 step 4). Only
such an echo sets the current endpoint, so the anchor moves to an endpoint of the
node only once that endpoint has echoed a fresh challenge. No other packet
changes it: a transport packet, relay envelope, or probe message from another
endpoint of the node, accepted or not, moves nothing. A challenge is used up by
the first accepted message that echoes it, whatever endpoint that message came
from.

An anchor has no current endpoint for a node until the first such echo on the
home session, and loses it when 75 seconds (3 reflection periods) pass without
one. While it has none, it drops the relay envelopes and signaling addressed to
that node and keeps the home session and its keys. The node sends a reflection
request at least every 25 seconds, so once its packets reach the anchor again
the next request's exchange sets a current endpoint, at most 25 seconds and two
round trips later, and relay delivery to the node resumes then. Per homed node
an anchor holds the current endpoint and at most 4 challenges it sent, each
until it is used up or 2 seconds old, the oldest dropped first at the limit
(section 7).

A node sends session traffic to an anchor, and an anchor to another anchor
(3.1.3), only at an address the control plane publishes for that anchor. That
address is never learned from a packet, so no packet moves where a node sends its
relayed traffic.

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
   an echo, the node handles the echo (step 6), which may verify the endpoint the
   message arrived from. Then, if it carries a challenge, the node answers it with
   exactly one answer, sent to the source endpoint the message arrived from,
   whether or not that endpoint is an advertised candidate of the sender, when
   that endpoint is now verified or the node is probing the sender. Otherwise the
   message is dropped silently. The answer echoes the challenge. It carries a
   challenge of its own only when it answers a probe, not an answer, that arrived
   from an endpoint that is not verified (step 6). A message without a challenge
   is never answered.
6. An **echo** from an endpoint E of a peer is an accepted probe message from E
   that echoes a challenge the node sent to E no more than 2 seconds earlier. A
   challenge is used up by the first accepted message that echoes it, whatever
   endpoint that message came from. A node holds each challenge it sent until it
   is used up or 2 seconds old, and then discards it. E **becomes verified** at
   an echo from E while E is not verified. The local candidate from which the
   node sent the challenge that this echo answers is then E's **near endpoint**,
   and stays so while E stays verified. The node sends every packet to E from
   E's near endpoint, except the probes of its probe sets on other candidate
   pairs (step 3) and its answers (step 5). E stays **verified** until the first
   moment when either of these holds:
   - E **fails**: three consecutive checks of the direct path to E are missed
     (section 3.4);
   - no `usable` path uses E, the node is not probing the peer, and the latest
     echo from E is 2 seconds old or older.

   A verified endpoint that is not an advertised candidate of the peer is a
   **peer-reflexive** candidate of that peer. Peer-reflexive candidates count
   toward the limit of 64 candidates per node (section 7); at the limit, a new one
   replaces the least recently verified peer-reflexive candidate of that peer, or
   is not recorded if there is none. A node never sends a probe without padding
   to a peer-reflexive candidate, in the probe set that verified it or in any
   later one, except the checks of section 3.4 while it is the far endpoint of a
   usable direct path. In every other respect a peer-reflexive candidate is a
   verified endpoint like any other.
7. A node sends probes without padding only to advertised candidates of the peer
   and, as the checks of section 3.4, to the far endpoint of a usable direct path.
   It sends padded probes (section 3.5) only to the far endpoint of a usable
   direct path. To an endpoint that is neither an advertised candidate of the
   peer nor verified, it sends only answers, at most one per accepted
   challenge-bearing message received from that endpoint, each no larger than
   that message, and the handshake replies of section 8 item 3. It sends
   transport packets (ARFC 0003 section 3.4) and handshake initiations (ARFC 0003
   section 3.1) directly to a peer only at endpoints it has verified.
8. A direct path to an endpoint E of the peer is `usable` at a node while E is
   verified and the node has accepted a transport packet directly from E, not
   inside a relay envelope, since E last became verified. It stops being usable,
   and is `failed`, when E stops being verified. A node keeps no record of
   transport packets from an endpoint that is not verified. So that each side
   receives such a packet, a node sends a keepalive (an AL packet of kind 0, ARFC
   0003 section 3.4) to an endpoint E of the peer at once when E becomes
   verified, at once when its direct path to E becomes usable, and to every
   verified endpoint of the peer in each later round of its probe sets toward
   that peer. Both conditions are things the node observes itself. Because
   transport packets go directly only to verified endpoints (step 7), a transport
   packet from E shows that the peer has verified an endpoint of this node and
   sends to it from E.
9. Among usable paths the active one is chosen by: direct before relayed. When
   no direct path is active and a direct path is usable, the usable direct path
   whose far endpoint echoed most recently becomes active. An active direct path
   stays active while it is usable. When it stops being usable, the usable direct
   path whose far endpoint echoed most recently becomes active at once, or the
   relay when none is usable. Nothing else changes the active path. A node
   therefore moves its session traffic to an endpoint only once that endpoint has
   echoed a fresh challenge and has sent it a transport packet since, and leaves
   an active direct path only when the path's far endpoint fails (step 6 and
   section 3.4). Its
   relayed traffic never moves: it goes only to an anchor's published address
   (section 3.1.2).

A node must be reachable through a relay within 2 seconds of joining, or within 5
seconds when no anchor answers it over UDP and it reaches them over the `stream`
underlay (section 3.6). A direct path, where section 3.3 expects one, must become
active within 5 seconds of first traffic. That bound assumes that the signal of
step 2 reaches B within 750 ms of A's first traffic and that the one-way delay
between A and B is at most 500 ms. Within these delays the two first probe sets
suffice: in cases 2 and 3 of section 3.3 the last endpoint a side needs becomes
usable at most five one-way delays and one probe round interval (200 ms) after
B's first probe, and at most 3.25 seconds after first traffic, and the 2 second
floor of step 6 keeps every verification it needs until then. A later signal can reach B after A's last probe round has
passed, and the two probe sets then do not overlap enough to verify the
endpoints that cases 2 and 3 need. Beyond these delays the bound does not apply,
and the pair may stay on the relay.

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

- A node **checks** each of its usable direct paths. A **check** of the path to
  the far endpoint E is a probe without padding, carrying a fresh challenge, that
  the node sends from E's near endpoint (section 3.2 step 6) to E while the path
  is usable. Probes to E from another local candidate, padded probes (section
  3.5), and answers are not checks, and their loss changes nothing here. The
  node sends a check whenever it has sent none on the path for 25 seconds. In
  each round of each of its probe sets toward the peer it sends one check on
  every usable direct path: the round's probe on the pair of E's near endpoint
  and E when E is an advertised candidate, and otherwise one probe more on that
  pair. The peer answers each check as section 3.2 step 5 says, and an echo
  keeps E verified. These checks are the path's keepalive: a NAT that refreshes
  its state on outbound packets, as RFC 4787 REQ-6 requires
  (`research/R-0003.md`), and whose mapping timeout is 30 seconds or more
  therefore never drops the state of an active direct path.
- A check is **outstanding** from when it is sent until it is echoed from E or
  missed. It is **missed** when 2 seconds pass after it was sent without an echo
  from E of its challenge, including when a message from another endpoint used
  the challenge up first. E **fails** at the moment a check is missed when the
  two checks the node sent on the path just before it were also missed and no
  check it sent on the path after it has been echoed. When a check is missed and
  E does not fail at that moment, the node sends another check at once if no
  check of the path is outstanding and it is not probing the peer. Whether a
  check is missed and whether E fails depend only on checks sent before that
  moment, so they do not depend on the order in which a node applies these
  rules at one instant.
- A direct path whose far endpoint fails is no longer usable and is `failed`.
  Traffic moves at once to the path section 3.2 step 9 chooses, the relay when
  no direct path is usable. A far endpoint that stops echoing therefore fails at
  most 31 seconds after the later of the last echo of one of its checks and the
  moment its path became usable (25 seconds to the next check, then three
  checks 2 seconds apart), and at most 2.4 seconds after the start of a probe
  set none of whose checks it echoes (three rounds 200 ms apart, the third
  missed 2 seconds after it was sent). A working path's far endpoint fails only
  when three consecutive checks of that path, each sent from its near endpoint
  without padding, go unechoed.
- Version 1 does not adapt the keepalive interval to the NAT. Behind a NAT whose
  mapping timeout is under 25 seconds, packets sent on an idle direct path may be
  lost until the path carries packets again or its far endpoint fails. The lab
  exercises this case with the preset in ARFC 0005 section 3.4.
- Each node sends a keepalive on each home session (section 3.1.2) whenever it
  has sent nothing on that session for 5 seconds. Behind a NAT that refreshes its
  state on outbound packets and whose mapping timeout is 10 seconds or more, the
  relay route to the node and its reflexive candidate therefore stay open however
  long the node is otherwise idle.
- While on a relay, direct probing repeats with backoff from 5 seconds to 5
  minutes. A change of either side's candidates starts a probe set at once,
  whether the session is on a relay or on a direct path.
- A node sends session traffic for a peer only on its active path: to the
  verified far endpoint of the active direct path, or through the relay. Session
  traffic is the session's transport packets and its handshake messages (ARFC
  0003 sections 3.1 and 3.4). Two kinds of packet go elsewhere: the keepalives of
  section 3.2 step 8, to verified endpoints of the peer, and a handshake response
  or cookie reply, to the source of the initiation it answers (section 8 item 3).
- An accepted packet from an endpoint of the peer that is not verified never
  changes where the node sends, and the node keeps no record of it. When it is a
  transport packet received directly, not inside a relay envelope, and the node
  is not probing that peer, it starts a probe set toward the peer. After a NAT
  rebinding of the peer, the peer's packets arrive from a new endpoint and start
  such a probe set. Its checks make the old far endpoint fail within 2.4 seconds
  if the rebinding closed it, and the path to the new endpoint becomes active by
  section 3.2 step 9 once the old one has failed and the new one is usable.
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
a usable direct path (section 3.2 step 7), from its near endpoint. They are not
checks: a padded probe that goes unechoed makes no far endpoint fail (section
3.4). Relayed paths use 1,280 until probed.
The payload budget is the path MTU minus the overhead in ARFC 0003.

### 3.6 Underlays

`udp` is the default. `stream` (TLS 1.3 over TCP port 443 to an anchor) is used
only for the home session and relayed traffic, and only after 3 seconds without a
UDP reply from any anchor, counted from the node's first UDP packet to an anchor.
Such a node must then be reachable through a relay within 5 seconds of joining
(section 3.2). A node on `stream` retries UDP every 5 minutes. On `stream` the
connection the node opened is the path. From the moment the home session on that
connection opens until the connection closes, the anchor sends the node's session
traffic, relay envelopes, and signaling only on that connection, with no echo
first. The current endpoint of section 3.1.2 and its 75 second lapse do not apply
to a node homed over `stream`, and no packet that arrives over UDP moves where
the anchor sends to it. Once the connection closes, the anchor sends nothing more
to the node on it, and a home session the node then opens over UDP follows
section 3.1.2.

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

Every table of this ARFC, its bound, and what happens at the bound:

| Table | Bound | At the bound |
|---|---|---|
| Candidates of a node, its own and those it holds for each peer, peer-reflexive included (section 3.2 step 6) | 64 | A new peer-reflexive candidate replaces the least recently verified peer-reflexive one of that peer, or is not recorded if there is none |
| Verified endpoints of a peer, each with its near endpoint, the time it became verified, its latest echo, and whether a transport packet arrived since it became verified (section 3.2 steps 6 and 8) | The peer's candidates, 64 | As for candidates |
| Outcomes of a usable direct path's checks that the fail rule reads (section 3.4): the send order of its outstanding checks, and whether each of its 3 latest checks in send order that are no longer outstanding was echoed or missed | Outstanding checks are held as probe challenges (row below); 3 resolved checks | The earliest resolved check is forgotten. A check whose challenge was dropped is missed 2 seconds after it was sent |
| Challenges a node sent in answers to one peer (section 3.2 step 5), each held until used up or 2 seconds old | 64 | The oldest is dropped; an echo of a dropped challenge verifies nothing |
| Challenges a node sent in probes to one peer, checks and padded probes included, each held until used up or 2 seconds old | 1,024 | As above |
| Probe set start times per peer within the last 5 minutes, and deferred probe sets (section 3.4) | 20, and 1 deferred set | Later triggers start no probe set and are served by the deferred one |
| Current endpoint of a homed node at an anchor (section 3.1.2) | 1 | A new echo replaces it |
| Challenges an anchor sent to one homed node, each held until used up or 2 seconds old (section 3.1.2) | 4 | The oldest is dropped, as above |
| Active sessions per node | 4,096 by default | The least recently used is closed |

A node keeps nothing for an endpoint of a peer that is not verified other than the
challenges it sent there, and an anchor nothing for an endpoint of a homed node
other than its current endpoint and those challenges. A packet from such an
endpoint is answered or starts a probe set, within the bounds above, and leaves no
other state. Relay forwarding per anchor is bounded by a configured rate, default
unlimited.

## 8. Security

Probes and signaling are authenticated. Probe messages and transport packets are
replay protected (ARFC 0003 sections 3.4 and 3.6), and only accepted packets
count in this ARFC (section 3.2 step 4): a replayed packet changes nothing.

A node sends to an anchor at an address the control plane publishes for it
(section 3.1.2), and on a `stream` connection it opened (section 3.6), the
packets ARFC 0003 and this ARFC call for. Toward any other endpoint that is not
verified (section 3.2 step 6), and that is not, at an anchor, the current
endpoint of a homed node (section 3.1.2), a node or an anchor sends only these
packets, and no others:

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
relay envelopes, signaling, checks, and padded probes, goes only to verified
endpoints (section 3.2 steps 7 and 8), to a homed node's current endpoint
(section 3.1.2), to anchors at their published addresses, and on a `stream`
connection the node opened. An accepted packet from an endpoint that is not
verified never changes where a node or an anchor sends, and leaves no state
beyond the bounded tables of section 7 (sections 3.1.2 and 3.4). At most it
starts a probe set toward the peer's advertised candidates, within the bound of
item 1. So a probe message that reaches a node from a spoofed source makes it
send at most one packet, of at most the message's size, to that source, only
while it is probing the message's sender; an anchor sends at most one such
packet. A transport packet from a spoofed source makes a node send nothing to
that source.

Traffic on a direct path whose far endpoint stops echoing ends when that
endpoint fails, at most 31 seconds after the later of the last echo of one of
its checks and the moment the path became usable (section 3.4). An
anchor's traffic to a homed node's current endpoint ends when another endpoint of
the node echoes, or at most 75 seconds after the node's last echo, when the
anchor loses the current endpoint (section 3.1.2).

A candidate is never trusted until a probe round trip succeeds. A peer,
compromised or not, can direct packets at a third party only by advertising its
address as a candidate, and then only as item 1 allows: from each node it has a
session with, at most 5 probe rounds per probe set and 20 probe sets in any 5
minutes. Upholds I-06, I-08, I-21.

### Moves need a fresh echo

A node or an anchor moves where it sends a peer's or a homed node's traffic only
on an echo, from the new endpoint, of a challenge it sent there no more than 2
seconds earlier. A node moves to a direct path whose far endpoint echoed and then
sent it a transport packet, and only from the relay or from a path whose far
endpoint failed (section 3.2 step 9). An anchor moves to the endpoint that echoed
most recently (section 3.1.2). No transport packet, relay envelope, or probe
message without such an echo moves anything, accepted or not, so the replay
window that relay envelopes lack (ARFC 0003 section 3.7) moves nothing here. A
challenge travels in the encrypted body of a probe message (ARFC 0003 section
3.6), so an attacker that cannot read a node's packets cannot produce an echo and
redirects nothing.

### Redirection by an underlay attacker

An echo shows that a challenge reached the peer and that an echo of it came back
from the endpoint. It does not show that the challenge went through that
endpoint. No field of a probe message binds the endpoint (ARFC 0003 section 3.6),
and no node can observe the endpoint its peer sends to, so none can be bound. An
underlay attacker (ARFC 0020 section 2.1) reads and injects. It can carry a
challenge exchange through any endpoint V it picks: it injects the node's message
to V at the peer, from a source the peer answers (section 3.2 step 5), and
injects the peer's answer at the node, from V. Each copy must be accepted in
place of the genuine packet: a copy of a packet the network dropped, or one that
overtakes the genuine packet, which the replay check then rejects. Copies that
arrive after the genuine packets are not accepted and change nothing. To the
node, the carried exchange is the one that case 3 of section 3.3 needs through a
NAT that maps the peer to V, so no rule can refuse it without closing that case.
Redirection by this attacker is therefore not nil. It is bounded as follows:

- Start at a node: V becomes usable after one carried exchange and one transport
  packet of the peer accepted from V (section 3.2 step 8). A node whose active
  path is the relay, as on every pair section 3.3 classes `relayed`, moves to V
  at once. A node whose active direct path works moves only once that path's far
  endpoint E fails, that is when three consecutive checks of the path are missed
  (section 3.4). Probes on other candidate pairs and padded probes do not count,
  so starting a probe set or a path MTU search fails nothing by itself. The
  attacker must keep the echo of each of those three checks from being accepted
  from E. Dropping the check or the answer does that, and so does overtaking:
  a copy of the peer's answer accepted from any other source before the genuine
  one uses the challenge up, and the genuine answer is then a replay (section
  3.2 steps 4 and 6). Overtaking drops no packet. Either way the third check is
  missed more than 2 seconds after the first was sent: 2.4 seconds within one
  probe set, whose rounds send a check every 200 ms, which the attacker can
  start with one transport packet of the peer accepted from a source that is
  not verified (section 3.4), and 6 seconds outside probe sets, where a check
  that is missed is followed by one retry 2 seconds after the one before.
- Start at an anchor: one carried reflection exchange, the node's request and its
  answer to the anchor's challenge each accepted from V ahead of the genuine one,
  makes V the node's current endpoint at once. A node homed over `stream` is not
  exposed: the anchor sends to it only on the connection it opened (section 3.6).
- Kind and rate: V receives only what the node sends to that peer on its active
  path, or the anchor to that homed node: session traffic, keepalives, checks,
  padded probes, relay envelopes, and signaling. The attacker cannot raise their
  rate.
- End at a node: the node checks V at least every 25 seconds and in every round
  of its probe sets toward the peer (section 3.4). Once the attacker stops
  carrying exchanges through V, V fails and the node stops sending to it at most
  31 seconds after the last echo of a check accepted from V.
- End at an anchor: at the node's next reflection exchange that completes from
  its own endpoint, at most 25 seconds and two round trips after the last echo
  accepted from V, or, if the attacker also keeps the node's own exchanges from
  completing, at most 75 seconds after that echo (section 3.1.2).

So the attacker keeps a redirection at a node only by carrying the exchange of
at least one of every three consecutive checks of V, at least once every 31
seconds, and at an anchor only by overtaking every reflection exchange of the
node, at least once every 25 seconds. It keeps it for as long as it does so, and no longer than the bounds
above.

The same attacker can make a node take V as its reflexive candidate (section
3.1.2), by getting a reflection request accepted from V in place of the node's
own, until the node's next reflection request is answered, at most 25 seconds
later. Peers then probe V only as item 1 allows, the bound above for a candidate
a peer advertises.

## 9. Open questions

- Port prediction for endpoint-dependent NAT pairs. Research task to be cut in M1.
