# ADR 0039: An authenticated probe is answered at the source it arrived from

- Status: accepted
- Subject: mesh
- Specs: arfc-0004, arfc-0005

## Context

ARFC 0004 probed only advertised candidates and was silent on a pair with one
endpoint-independent side and one endpoint-dependent side. Behind an
endpoint-dependent NAT a probe leaves from a port nobody advertised, so such pairs
reach a direct path without port prediction only if the receiver answers at the
observed source. Answering must not let a replayed or spoofed probe amplify.

## Decision

Every probe message carries a challenge, an echo, or both. A node answers each
authenticated challenge-bearing message once, at its source, with an answer no
larger than it. The answer carries its own challenge only when it answers a probe
from an unverified endpoint. An endpoint is verified when it echoes a challenge
sent to it. A node never probes an endpoint that is neither advertised nor
verified, and answers unverified endpoints only while probing the sender. The
path class follows ARFC 0004 section 3.3.

## Consequences

- 34 of the 81 single-device mapping and filtering combinations are direct, as
  the three cases of ARFC 0004 section 3.3 state; 55 of R-0003's 144 preset pairs.
- No amplification: per challenge-bearing message from an unverified endpoint, a
  node sends at most one packet of at most its size, and nothing outside a probe
  set. Probes have no replay check, so a replayed probe is limited by this bound.
- ARFC 0003 section 3.6 must give probe messages challenge and echo fields.
- Repeated probing is signaled so both sides run a probe set at once.

## Alternatives rejected

- Probe only advertised candidates: relays pairs that need no port prediction.
- Probe each new source for 5 rounds: a replayed probe makes 6 packets.
- Answer with a reply and a separate probe: 2 packets per probe received.
- Port prediction now: costly and deferred to M1 by ARFC 0004 section 9.
