# ADR 0061: Traffic moves only to an endpoint that echoed a challenge

- Status: accepted
- Subject: mesh
- Specs: arfc-0004

## Context

This ADR supersedes ADR 0049, which moved traffic by timing: to the endpoint heard most recently once the one in use was silent for 1 s. Each review of that rule found a hole: a node on the relay moved at once on four copied packets, `usable` needed an unbounded 75 s record of transport packets from endpoints not yet verified (I-21), and anchors heard relay envelopes, which have no replay window.

## Decision

Only an echo of a fresh challenge moves traffic. A node keeps an active direct path while it is usable and leaves it only when the far endpoint fails, that is when 3 consecutive checks of the path go 2 s unechoed. A check is a probe without padding sent on the path itself, from the near endpoint, the local candidate whose challenge verified the far endpoint; probes on other candidate pairs and padded probes are not checks. Whether a check is missed and whether the endpoint fails depend only on checks already sent, so no order of rules at one instant changes them. The node then takes the usable path that echoed most recently, or the relay. `usable` needs a transport packet from E since E became verified, and a node sends a keepalive when E becomes verified and when the path becomes usable. Usable paths are checked: a check every 25 s, one more at once when a check is missed and none is outstanding, and one in each probe set round; the checks are the path keepalive. An anchor sends to a node homed over UDP only at the endpoint that most recently echoed one of its challenges, every reflection answer carries one, and it drops that endpoint after 75 s without an echo. To a node homed over `stream` it sends only on the connection the node opened, with no echo and no lapse. The RTT switching rule is dropped. Nothing is kept for an endpoint that is not verified, and every table has a bound (ARFC 0004 section 7).

Kept from ADR 0049: only accepted packets count, a challenge is used up by its first echo, verification lasts at least 2 s, the probe set caps, and only unpadded probes to advertised candidates, answers and handshake replies reach an endpoint that is not verified.

## Consequences

- No transport packet or relay envelope moves anything, so an attacker that cannot read a node's packets redirects nothing.
- An underlay attacker that reads and injects can still carry a challenge exchange through an address V and redirect to it: at once on the relay, and on a working direct path only after the echoes of 3 consecutive checks of that path are not accepted from its far endpoint, which the attacker gets by dropping them or, dropping nothing, by overtaking the answers with copies from another source. It ends 31 s at a node, and 25 s at an anchor (75 s if blocked), after the attacker's last carried echo. No rule can refuse that exchange, since case 3 of ARFC 0004 section 3.3 needs the same one. ARFC 0004 section 8 states the bound.
- ADR 0039's counts hold: 34 of 81 single and 872 of 7,290 layered cases direct.
- The 5 s direct bound holds; the last needed endpoint is usable at most five one-way delays and one probe round after the signal arrives, at most 3.25 s after first traffic.
- A far endpoint that stops answering fails within 31 s, not 75 s. After a NAT rebinding traffic moves 2.4 s after the new endpoint's first packet starts a probe set. A node with several local candidates, or in a path MTU search, does not lose a working path to probes that cannot reach the far endpoint. Relay delivery to a node may pause up to 25 s after its own rebinding.
- Each usable path costs one probe and one answer per 25 s.

## Alternatives rejected

- Bind the endpoint into the echo: no node sees the address it is reached at behind a NAT.
- Move to whichever endpoint echoed last: one carried exchange moves a working path, no blocking.
- Count every challenge sent to the far endpoint toward its failure: probes on other candidate pairs and padded probes then fail a working path with no loss.
- Refuse answers to unverified sources that are not advertised: closes case 3 of ARFC 0004 section 3.3.
- Bound the record of pre-verification packets: changes the class counts and keeps the relay attack.
