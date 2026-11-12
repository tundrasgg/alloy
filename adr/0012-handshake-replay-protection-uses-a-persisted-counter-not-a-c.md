# ADR 0012: Handshake replay protection uses a persisted counter, not a clock

- Status: accepted
- Subject: mesh
- Specs: arfc-0003

## Context

Small machines often boot with a wrong clock, which breaks timestamp-based replay protection.

## Decision

Each node keeps a monotonic persistent handshake counter, reserved in blocks. Responders reject counters that do not increase.

## Consequences

- Joining and reconnecting work with no time source.
- One synced write per 65,536 handshakes.

## Alternatives rejected

- Timestamps: fail on machines without a battery-backed clock.
