# ADR 0063: Path checks are held apart from probes, and traffic moves only on an echo

- Status: accepted
- Subject: mesh
- Specs: arfc-0004

## Context

This ADR supersedes ADR 0061. T-0090 changed ARFC 0004 after ADR 0061 was accepted: checks got their own table, a probe budget bounds every peer, and the 5 s direct bound gained a condition. Its review (T-0090 round 2, F1) found ADR 0061 no longer true as written, since it names only dropping and overtaking as ways to make checks go unechoed. F2 found that measuring lateness at arrival let a stalled receiver break the 2,004 bound with nothing late. The T-0088 work found Alloy's own addresses could become candidates.

## Decision

- Kept from ADR 0061: only an echo of a fresh challenge moves traffic. A node keeps an active direct path while it is usable and leaves it only when the far endpoint fails, after 3 consecutive checks of the path go 2 s unechoed. A check is an unpadded probe on the path from its near endpoint. `usable` needs a transport packet from E since E became verified. Checks run every 25 s, once more at a miss with none outstanding, and once per probe set round. An anchor sends to a node homed over UDP only at the endpoint that last echoed its challenge, for at most 75 s after that, and to a node homed over `stream` only on its connection. Nothing is kept for an unverified endpoint, and every table has a bound.
- New: a path's outstanding checks hold their challenges in their own table, at most 11 per path, 704 per peer, so no other probe displaces one. When E stops being verified, the node forgets the path's checks, so a path usable again fails only on 3 consecutive misses since then.
- New: a probe budget per peer over any 2 s: checks of the active path and checks outside rounds without limit (at most 74), 60 other checks, 20 padded probes, 180 other probes. At most 334 probes and, while no probe message is late, 2,004 probe messages per peer in 2 s, so the 2,048-counter replay window rejects no check or answer to one.
- New: a probe message is late when the node accepts it 2 s or more after it was sealed, however long it waited at the node, and a node seals an answer at the moment it accepts what it answers. Delay that makes messages late is a third way, beside dropping and overtaking, for an underlay attacker to make checks missed. The bounds of ARFC 0004 section 8 are unchanged.
- New: the 5 s direct bound holds when the signal reaches the peer within 750 ms, the one-way delay is at most 500 ms, and each probe set covers at most 18 candidate pairs.
- New: no address on an interface whose name ARFC 0017 clause 3.17 reserves, inside the infrastructure prefix, loopback, or link-local is ever a local candidate.
- New: a session an unregistered initiator opens to an anchor's join service follows the home session rule: the anchor sends on it only at the initiator's current endpoint.

## Consequences

- ADR 0061's consequences hold, except that the 5 s bound now carries the 18-pair condition and checks can also be missed through late messages.
- A probe set of any size, or a path MTU search, fails no working path by itself.
- A node behind its own `al0` or VPC interfaces never probes through them, so AL is never carried inside AL.
- A joiner exchanges one reflection request before its first node message, as a homed node does.

## Alternatives rejected

- Measure lateness at arrival: a stalled receiver then seals answers to messages from spans over 4 s while nothing is late (T-0090 round 2, F2).
- One challenge table for checks and probes: a large probe set displaced checks and failed working paths (D-0038).
- Exclude only `al0`: `alv` interfaces and infrastructure addresses elsewhere carry AL too.
- Let a join session's replies follow the latest packet's source: one copied packet would aim the bootstrap view at a third party.
