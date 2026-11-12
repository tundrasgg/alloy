# ADR 0013: Fragmentation happens inside AL, never in IP

- Status: accepted
- Subject: mesh
- Specs: arfc-0003, arfc-0009

## Context

Tenants sometimes need a 1,500 byte MTU, and IP fragments are widely dropped.

## Decision

AL fragments and reassembles inner packets itself. Underlay packets never exceed the path MTU and never rely on IP fragmentation.

## Consequences

- A VPC can offer 1,500 bytes over a 1,500 byte underlay, at a throughput cost.
- Reassembly memory is bounded per peer and per node.

## Alternatives rejected

- Reduced MTU only: breaks software that assumes 1,500.
