# ADR 0038: Seccomp is fixed per hypervisor, Landlock and namespaces per guest

- Status: accepted
- Subject: compute
- Specs: arfc-0011, arfc-0020

## Context

ARFC 0011 section 2 required a seccomp filter that allows "only what that guest's devices need". Cloud Hypervisor's filter is built in and fixed, not generated per guest, while its Landlock rules are passed per VM and must all be known at start (`research/R-0002.md`, sources 8 and 13). Landlock does not cover AF_UNIX sockets, so namespaces are needed alongside it (R-0002, source 11). ARFC 0011 section 3.1.4 requires hot attach of volumes and ENIs.

## Decision

- The seccomp filter of a VMM process is fixed per `hypervisor` implementation and version, enforcing (a violation ends the process), on every thread. Alloy adds no filter of its own and never runs a VMM with seccomp disabled, logging, or returning errors.
- Per-guest confinement comes from the guest's own Unix user, cgroup, user, mount, and network namespaces, and a Landlock rule set fixed at start that grants the guest directory read-write and the guest's boot files read-only.
- Every Unix socket the VMM uses, including every vhost-user socket for a hot attached volume or ENI, lives in the guest directory, so hot attach never needs a new Landlock rule.
- Unix sockets are confined without Landlock: the mount namespace shows only the guest's own socket files, each socket file grants write permission (needed to connect, https://man7.org/linux/man-pages/man7/unix.7.html) to no other guest's user, and the network namespace isolates abstract sockets (https://man7.org/linux/man-pages/man7/network_namespaces.7.html).
- The VMM's network namespace has no interface other than loopback. Migration traffic leaves through a socket in the guest directory.

Refines ADR 0022 without superseding it: each VMM still has its own filter instance.

## Consequences

- ARFC 0011 section 2 no longer asks for a capability that R-0002 finds missing from the first implementation.
- What differs between guests is checkable from outside: user, namespaces, mount view, socket modes, Landlock rules.
- A filter regression in a VMM release is caught only by that release's own filter, not by Alloy.
- Section 3.5 migration must use a Unix socket relayed by Alloy over the mesh, not a VMM TCP listener.

## Alternatives rejected

- An outer per-guest seccomp filter written by Alloy: needs the VMM's syscall set per release, unsourced, and a wrong entry kills guests.
- Asking the VMM for a filter derived from the guest's devices: the first implementation has no such option (R-0002, source 13).
- Adding Landlock rules at hot attach: Landlock rules cannot be added after start (R-0002, source 8).
- Relying on Landlock for sockets: it does not cover AF_UNIX (R-0002, source 11).
