# ADR 0022: One sandboxed VMM process per guest, Cloud Hypervisor first

- Status: accepted
- Subject: compute
- Specs: arfc-0011, arfc-0020

## Context

The hypervisor process is the main surface a hostile guest attacks.

## Decision

Each guest gets its own VMM process under its own user, namespaces, cgroup, seccomp filter, and Landlock policy. The first `hypervisor` implementation is Cloud Hypervisor, run as a separate program.

## Consequences

- A VMM escape lands in an empty sandbox.
- Other hypervisors can be added behind the seam.

## Alternatives rejected

- A shared hypervisor daemon: one compromise reaches every guest on the node.
