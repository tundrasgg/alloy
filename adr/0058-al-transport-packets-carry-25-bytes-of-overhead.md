# ADR 0058: AL transport packets carry 25 bytes of overhead

- Status: accepted
- Subject: mesh
- Specs: arfc-0003, arfc-0004, arfc-0009

## Context

AL transport packets carried 36 bytes of overhead: a 16-byte header, a 4-byte
inner kind and VNI, and a 16-byte tag. The pilot directed five cuts and no
others (D-0024). ARFC 0003 is not frozen, so the layout can still change cheaply.

## Decision

- The header is `type (1), receiver_index (4), counter (4)`. Type 4 is transport
  under a version 1 session; first byte values 0 and 7 to 255 are reserved.
- An index names session, VNI and queue: 64 per VNI per session, VNI 0's from
  the handshake, others bound per session by control frames. At most 1,024
  blocks per session and 4 sessions per peer.
- The wire counter is the low 32 bits of the per-queue counter, reconstructed
  from the replay window. The nonce keeps the full counter. Rekeying at 2^30
  packets per queue and a hard stop at 2^31 make the reconstruction exact.
- The inner frame's kind is its first 4 bits: 4 IPv4, 6 IPv6, 0 keepalive, 1
  control, 2 fragment. Fragments count from 0, carry IP or control frames, and
  have a 7-byte header with no reserved byte.

## Consequences

- Overhead is 25 bytes: 53 with UDP over IPv4, 73 over IPv6. Fragments add 7.
- Nonces stay unique per key (I-07). The VNI is authenticated through the index.
- A VNI's first packets to a peer wait one bind round trip (16 held, 1 second);
  after a rekey a VNI stays on the old session until bound anew.
- An initiator sends a keepalive on each new session so the responder confirms.
- `alloy-wire` (T-0054) and its vectors follow the old layout: recut them.

## Alternatives rejected

- Shorter tags, 2-byte counter, indexes in ports, raw IP: not chosen (D-0024).
- Bindings per peer across sessions: a rebound index could misdeliver (I-01).
- Index derived from the VNI by hash: collides across a node's sessions.
