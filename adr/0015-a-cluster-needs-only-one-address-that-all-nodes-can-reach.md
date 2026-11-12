# ADR 0015: A cluster needs only one address that all nodes can reach

- Status: accepted
- Subject: mesh
- Specs: arfc-0000, arfc-0004

## Context

Many deployments have a single reachable machine and the rest behind NAT.

## Decision

One reachable address, on any network, is enough to form and run a cluster. Running with a single anchor is supported and carries a standing warning.

## Consequences

- The join token alone bootstraps connectivity.
- A single anchor is a single point of failure for joins, relays, and ingress through it.

## Alternatives rejected

- Require several reachable nodes: excludes the smallest deployments.
