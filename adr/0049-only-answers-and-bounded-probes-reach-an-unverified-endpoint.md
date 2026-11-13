# ADR 0049: Only answers and bounded probes reach an unverified endpoint

- Status: accepted
- Subject: mesh
- Specs: arfc-0004

## Context

ARFC 0004 moved a session's return path to any new authenticated source at once,
so one reinjected transport packet sent a session's traffic to a third party.
`usable` needed a round trip "in both directions" that no node can observe, and
probe sets restarted on every candidate change with no limit. No probe field
binds an address, so an attacker that reads and injects can fake verification.

## Decision

Only accepted packets (authenticated, past the replay check) count. Session
traffic, keepalives, relay envelopes and padded probes go directly only to
endpoints the sender verified (ARFC 0004 section 3.2 step 6); elsewhere a node
sends only unpadded probes to advertised candidates, answers, and handshake
replies. A challenge is used up by its first echo. Verification lasts at least
2 s. A direct path is usable while its far endpoint is verified and has sent a
transport packet within 75 s. A node or anchor leaves the endpoint it sends to
only after 1 s without a transport packet from it, for the endpoint it heard
most recently. A packet from an unverified endpoint never moves where anything
is sent. Anchors verify homed endpoints through reflection requests every 25 s,
keep 4, and close a home session after 15 s of silence. Probe sets per peer are
capped at 2 per 5 s and 20 per 5 min. Peer-reflexive candidates get no unpadded
probes.

## Consequences

- A copy that arrives after the genuine packet, replays included, makes a node
  send nothing new; a spoofed transport packet makes it send nothing to the
  spoofed source.
- An underlay attacker that overtakes packets, or delivers ones the network
  dropped, can still redirect a node's or an anchor's traffic for one peer to an
  address it picks. It must first stop the endpoint in use being heard for 1 s,
  and the redirection ends 1 s after its last such packet once the real endpoint
  is heard again: at most 5 s later at an anchor (25 s if the real endpoint was
  displaced) and 75 s at a node. ARFC 0004 section 8 states this bound; nothing
  here claims more.
- ADR 0039's counts hold: 34 of 81 single and 872 of 7,290 layered cases direct.
- The 5 s direct bound holds at 750 ms signal delay and 500 ms one-way delay.
- After a NAT rebinding traffic moves 1 s after the old endpoint goes silent;
  relay traffic to a node may pause up to 25 s.

## Alternatives rejected

- Bound traffic after a move, or probe the new source: no bound, or amplifies.
- Probe verified peer-reflexive candidates: opens 6 of 81 and 244 layered cases.
- Bind the address in the probe seal: a node cannot see the address it is
  reached at behind a NAT, and the wire format belongs to ARFC 0003.
- Reset `usable` at each probe set: a few overtaken packets move a heard path.
