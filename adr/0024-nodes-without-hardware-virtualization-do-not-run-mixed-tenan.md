# ADR 0024: Nodes without hardware virtualization do not run mixed tenants

- Status: accepted
- Subject: compute
- Specs: arfc-0011

## Context

Some machines cannot run VMs, yet are useful.

## Decision

Such nodes may be anchors, control, and storage. They run containers only when dedicated to one organization, through the `sandbox` seam. Instances are refused there.

## Consequences

- Small boards and nested hosts still contribute.
- The tenant boundary never rests on namespaces alone.

## Alternatives rejected

- Software emulation: too slow to offer.
