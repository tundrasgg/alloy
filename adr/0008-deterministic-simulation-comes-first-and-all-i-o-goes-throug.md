# ADR 0008: Deterministic simulation comes first and all I/O goes through alloy-rt

- Status: accepted
- Subject: engineering
- Specs: arfc-0005

## Context

Distributed systems bugs depend on timing and failure order, which ordinary tests do not control.

## Decision

All I/O is behind `alloy-rt` traits. A seeded single-threaded simulator implements them. The simulator and NAT lab are built before any feature.

## Consequences

- Whole clusters and hostile networks are tested from a seed, reproducibly.
- Direct calls to the clock, network, disk, or randomness are rejected by lint.

## Alternatives rejected

- Integration tests on real machines only: slow, flaky, and unable to reach rare interleavings.
