# ADR 0031: Scale comes from lazy sessions, scoped views, and regions

- Status: accepted
- Subject: architecture
- Specs: arfc-0000, arfc-0004, arfc-0006

## Context

A full mesh and a full copy of state on every node stop working at a few hundred nodes.

## Decision

Nodes open sessions only when they have traffic. Nodes receive only the state they serve. Regions have separate consensus groups. Storage placement is computed, not looked up.

## Consequences

- Per-node cost depends on what the node serves, not on cluster size.
- Design targets are checked by simulation.

## Alternatives rejected

- Full mesh and full state replication: simple and bounded to small clusters.
