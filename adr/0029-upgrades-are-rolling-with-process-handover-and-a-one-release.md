# ADR 0029: Upgrades are rolling, with process handover and a one-release skew

- Status: accepted
- Subject: operations
- Specs: arfc-0017

## Context

A cloud that stops guests to upgrade will not be upgraded.

## Decision

Adjacent minor releases interoperate. Dataplane processes hand over sockets and state to their successors. New formats are written only after finalization.

## Consequences

- Guests keep running through upgrades.
- Every format and protocol carries a version from the first commit.
- Rollback is possible until finalization.

## Alternatives rejected

- Restart-everything upgrades: downtime per release.
