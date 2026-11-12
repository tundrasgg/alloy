# ADR 0010: Alloy has its own overlay protocol and a userland dataplane

- Status: accepted
- Subject: mesh
- Specs: arfc-0003, arfc-0004

## Context

Path probing has to share the data socket, and the wire format has to carry tenancy and fragmentation.

## Decision

Nodes speak AL, Alloy's own protocol, implemented in userland in `alloy-net`. Interfaces are named `al*`.

## Consequences

- Probes, relay, VNI, and fragmentation are first class in the format.
- Performance work (batching, offloads, kernel bypass) is ours to do.

## Alternatives rejected

- An existing kernel tunnel: it owns its socket and its header.
