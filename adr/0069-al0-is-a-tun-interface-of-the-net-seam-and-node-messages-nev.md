# ADR 0069: al0 is a tun interface of the net seam, and node messages never pass through it

- Status: accepted
- Subject: network
- Specs: arfc-0002, arfc-0005, arfc-0009

## Context

M11 rung 3 needs `al0` (ARFC 0002 section 3.4) on Linux nodes and on a Windows client node. Windows has no built-in layer 3 virtual interface, and ADR 0062 admits Wintun for it. Linux offers TUN, a kernel device that carries bare IP packets to a user process and that the kernel removes, with its routes, when the process's descriptor closes (<https://docs.kernel.org/networking/tuntap.html>). All I/O goes through `alloy-rt` and must run on the simulator (ADR 0067). The interface is also the one way an unprivileged local program could inject packets into the mesh under the node's address.

## Decision

This replaces the last clause of ADR 0042's second bullet, "then the same packets ride `al0`". Node messages keep the bytes and carriage of ADR 0042 and ADR 0065 from rung 3 on, beside `al0`. Every other part of ADR 0042 that ADR 0065 left standing stands.

- `al0` is a tun interface made by `Net::create_tun` (ARFC 0005 clause 3.1.17): a TUN device with `IFF_TUN | IFF_NO_PI | IFF_TUN_EXCL` on a close-on-exec descriptor on Linux, a Wintun adapter on Windows, and a host-local packet queue under `mem`. `create_tun` refuses a name another object holds with `Conflict`, except that on Windows it takes over a Wintun adapter whose interface GUID is the one Alloy's host file records for it. It is part of the `net` seam, not a new seam, and is tested by the `net` suite.
- `add_addr` and `add_route` refuse, with `Conflict`, a prefix that overlaps a route or address of the host not on the interface, which is ARFC 0017 clause 3.17.3's check. Prefix length 0 is never routed through it.
- The interface has no link-local address: `addr_gen_mode` 1 on Linux, `LinkLocalAlwaysOff` on Windows. Addresses skip duplicate address detection.
- `al0` has MTU 1,395, so a 1,500-byte path carries every packet of `al0` unsplit, direct or relayed, over either IP family.
- Only IPv6 packets between the node's infrastructure address and that of a node in its view cross `al0`, on VNI 0. An inbound source must be the address of the mesh key that authenticated the session.
- The node never takes a node message (UDP port 7477) from `al0` and never writes one to it. `alloyd` makes and takes node messages itself. This rules on ARFC 0002 section 3.13, whose "travel through it" now reads "beside `al0`".
- `alloyd` holds `al0` in this version. How `al0` passes to `alloy-net` and across its handover is left to the spec task that moves AL sessions there.
- ARFC 0009 clause 3.13 is where client nodes get VPC routes through `al0`. Rung 3 gives them none.

## Consequences

- The simulator runs `alloyd`'s `al0` path. It carries UDP and no stream over a tun interface, so a later service that runs TCP over `al0` needs a `mem` stream model there first.
- A local program cannot forge or read node messages through `al0`. Every other service on the infrastructure network must authenticate peers itself, never by source address alone.
- Alloy adds no firewall state. What listens on a node's addresses is reachable from the cluster's nodes over `al0`.
- Whether a Wintun adapter outlives a crashed process is not documented (ARFC 0005 section 9). If it does, the next `create_tun` with its GUID takes it over. Wintun removes only adapters that its handle created, so a taken-over adapter stays after `remove` until the uninstall removes it.
- Until views are built, `al0` carries packets only between anchors and control nodes (ARFC 0002 section 3.4).
- The `os` tests of `create_tun` need root or elevation, so they run privileged on the CI hosts and fail there when they cannot.

## Alternatives rejected

- A TAP device or a layer 2 adapter: `al0` is layer 3, and a link layer would add neighbor discovery and a MAC for no use.
- A seam of its own: the tun interface shares the host's interface list, routing, and sockets with `net`, and `mem` must route sockets through it.
- MTU 1,280, the IPv6 minimum: it costs every packet 115 bytes that a 1,500-byte path carries anyway. AL fragmentation covers smaller paths.
- Node messages through `al0` with a socket on port 7477: any local program could then bind the port or send from it and act as the node.
