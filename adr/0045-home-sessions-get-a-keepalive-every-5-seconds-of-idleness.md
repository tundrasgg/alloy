# ADR 0045: Home sessions get a keepalive every 5 seconds of idleness

- Status: accepted
- Subject: mesh
- Specs: arfc-0004, arfc-0005

## Context

ARFC 0004 kept direct paths alive with a 25 s keepalive and said nothing about
home sessions. A NAT drops the state toward the anchor after its mapping timeout,
10 s at the shortest in `research/R-0003.md`, which closes the relay route to an
idle node and invalidates its reflexive candidate. The relay is the fallback when
a direct path fails, so it must outlive the shortest timeout.

## Decision

Each node sends a keepalive (AL kind 0) on each home session whenever it has sent
nothing on that session for 5 seconds. The direct path keepalive stays 25 s.

## Consequences

- Behind a NAT that refreshes on outbound packets with a timeout of 10 s or more,
  the whole range ARFC 0005 section 3.4 models, the relay route and the
  reflexive candidate stay open.
- The `short-timeout` lab expectation, relay delivery after a direct path fails,
  follows from ARFC 0004.
- Cost: at most 2 home sessions per node, so at most 0.4 packets per second per
  node, and an anchor with N homed nodes receives at most N / 5 per second.

## Alternatives rejected

- 25 s like direct paths: the relay route closes behind NATs under 25 s.
- Adapt the interval to a measured NAT timeout: more state, no data yet.
- Limit the lab expectation instead: leaves the fallback unreliable.
