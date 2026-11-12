# ADR 0014: Traffic starts on a relay and upgrades to a direct path

- Status: accepted
- Subject: mesh
- Specs: arfc-0004

## Context

Path discovery takes time and sometimes fails. The first packet should not wait.

## Decision

Anchors relay. A pair of nodes sends through a relay immediately, probes for a direct path in parallel, and moves to it when found.

## Consequences

- Connectivity is immediate and never depends on NAT behavior.
- Anchors need capacity for relayed traffic.

## Alternatives rejected

- Wait for a direct path before sending: slow starts and hard failures behind strict NAT.
