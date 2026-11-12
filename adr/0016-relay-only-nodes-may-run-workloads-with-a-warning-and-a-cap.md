# ADR 0016: Relay-only nodes may run workloads, with a warning and a cap

- Status: accepted
- Subject: mesh
- Specs: arfc-0004

## Context

Some nodes will never get a direct path. Refusing them wastes capacity.

## Decision

A relay-only node may hold any role. It is marked in status, shown in the CLI and console, scheduled last, and its relayed traffic is capped by a cluster setting.

## Consequences

- Anchors are protected from one node's traffic.
- Users see why such a node is slow.

## Alternatives rejected

- Refuse workloads on relay-only nodes: surprising and wasteful.
