# ARFC 0003: AL, the mesh wire protocol

- Status: accepted
- Depends on: 0000
- Crates: alloy-wire, alloy-link, alloy-crypto
- Decisions: 0010, 0011, 0012, 0013, 0030, 0043, 0046, 0057, 0058

## 1. Purpose

AL (Alloy Link) is the protocol between nodes. It gives authenticated, encrypted,
replay-protected datagrams tagged with a VPC, over UDP or a stream, with its own
fragmentation. This ARFC defines its bytes and state machines. Path selection is
ARFC 0004.

## 2. Model

A **session** exists between two node mesh keys. It is created by a two-message
handshake and holds one sending key and one receiving key per direction. A session
is independent of paths: any packet that authenticates belongs to the session
whatever underlay address it came from. At each node, a session's transport
packets arrive on receiver indexes, one per sending queue of the peer and VPC
carried (3.4).

All integers are little endian. All messages start with a 1-byte `type`. Every
message except a transport packet then carries a 1-byte `version`, currently
`1`. A transport packet has no `version` byte: its session's version is fixed by
the handshake, and its `type` names the version (3.4).

## 3. Behavior

### 3.1 Handshake

The handshake is the Noise pattern `IKpsk2` (Noise specification revision 34,
https://noiseprotocol.org/noise.html), instantiated as `X25519`,
`XChaCha20-Poly1305`, `BLAKE2s`:

```
IKpsk2:
  <- s
  ...
  -> e, es, s, ss
  <- e, ee, se, psk
```

- Protocol name: the 21 ASCII bytes `alloy/al/handshake/v1`, used where Noise
  uses the protocol name. It is shorter than 32 bytes, so the initial `h` is the
  name followed by 11 zero bytes.
- Prologue: zero length.
- Nonce: where Noise calls for a 64-bit nonce `n`, the 24-byte AEAD nonce is 16
  zero bytes followed by `n` as 8 bytes little-endian.
- Pre-shared key: 32 bytes, mixed at position `psk2`, that is at the end of the
  response. A responder chooses it by the initiator static key it learned from
  the initiation, so a Noise library is handed the key between the two messages.
  When no pre-shared key is configured for a pair of nodes, the key is 32 zero
  bytes. Both sides must use the same key or the initiator fails to decrypt the
  response payload and the handshake fails.

The Noise message bytes are exactly the fields `ephemeral`, `encrypted static`
and `encrypted payload` of the initiation (112 bytes) and `ephemeral` and
`encrypted payload` of the response (52 bytes), in that order. All other fields
are outside Noise: they are not in the handshake hash and the handshake does not
authenticate them (3.3). Payloads are encoded with the field order and sizes
shown, little-endian.

Notation: `BLAKE2s(x)` is unkeyed BLAKE2s with a 32-byte output. `MAC(key, x)`
is keyed BLAKE2s (RFC 7693) with a 16-byte output and `key` as the key. `||` is
concatenation.

Initiation (type 1), 152 bytes:

| Field | Size |
|---|---|
| type, version, reserved | 1, 1, 2 |
| sender_index | 4 |
| ephemeral | 32 |
| encrypted static | 32 + 16 |
| encrypted payload | 16 + 16 |
| mac1, mac2 | 16, 16 |

The payload is `{counter: u64, queues: u16, flags: u16, reserved: u32}`.
`counter` is the initiator's handshake counter (3.2). `queues` is the number of
sending queues it will use, 1 to 64. Flag bit 0 is reserved for a hybrid key
exchange (ADR 0030).

Response (type 2), 96 bytes:

| Field | Size |
|---|---|
| type, version, reserved | 1, 1, 2 |
| sender_index | 4 |
| receiver_index | 4 |
| ephemeral | 32 |
| encrypted payload | 4 + 16 |
| mac1, mac2 | 16, 16 |

The payload is `{queues: u16, flags: u16}`, with the meanings above for the
responder.

Cookie reply (type 3), 64 bytes:

| Field | Size |
|---|---|
| type, version, reserved | 1, 1, 2 |
| receiver_index | 4 |
| nonce | 24 |
| encrypted cookie | 16 + 16 |

Indices: a node chooses the `sender_index` of each initiation and response it
sends so that the 64 values `sender_index + q` (mod 2^32), `q` from 0 to 63, are
all free at that node: none of them is in a block (3.4) of a pending handshake
or of a session the node holds. The block stays reserved while the initiation is
pending and, once the handshake makes a session, for as long as the node holds
that session. The peer copies `sender_index`, unchanged, into the
`receiver_index` of every response and cookie reply it sends to that node and of
every probe message (3.6) it seals in the session. Transport packets use the
indexes of the block, and of further blocks, as 3.4 defines. A node that has no
free block when it must reserve one does not send that initiation or response,
and counts it.

Fixed values: in version 1, `version` is 1, the header `reserved` bytes are
zero, every flag bit is zero, and the initiation payload `reserved` is zero. A
receiver must drop, without reply, a message of this section whose `version` is
not 1, whose header `reserved` is not zero, or whose length is not the one shown
for its type. A responder must drop an initiation, without reply, whose
decrypted payload has `queues` outside 1 to 64, a flag bit set, or a non-zero
`reserved`. An initiator must abandon a handshake whose decrypted response
payload has `queues` outside 1 to 64 or a flag bit set.

MACs: `mac1` is `MAC(BLAKE2s("alloy/al/mac1/v1" || S), m1)`, where `S` is the
32-byte static public key of the node the message is sent to (the responder for
an initiation, the initiator for a response) and `m1` is every byte of the
message before `mac1`. A receiver must check `mac1` before any other
cryptographic operation and drop the message, without reply, if it does not
verify. `mac2` is `MAC(cookie, m2)`, where `m2` is every byte of the message
before `mac2`, `mac1` included, and `cookie` is the cookie the initiator holds
from that responder. An initiator that holds no cookie for the responder sets
`mac2` to 16 zero bytes. A response always carries 16 zero bytes in `mac2`, and
an initiator must drop a response whose `mac2` is not zero.

Cookies: a responder holds a 32-byte `cookie_secret`, drawn from the `alloy-rt`
randomness trait and replaced by a fresh draw every 120 seconds. The cookie for
an initiation is `MAC(cookie_secret, src)`, where `src` identifies where the
initiation came from:

- UDP: the source IP address (4 bytes for IPv4, 16 for IPv6) followed by the
  source port (2 bytes), both in the byte order of the IP and UDP headers.
- Stream underlay (3.8): the remote address and port of the stream, in the same
  form.
- Inside a relay envelope (3.7): the `src` of the envelope followed by its
  `relay_handle` (4 bytes, as on the wire).

Only the responder computes and checks its cookies, so `src` and the cookie
never need to agree between implementations. They are fixed here so that one
implementation can be tested against vectors. A `mac2` is valid when it verifies
under the cookie computed from the current `cookie_secret` or from the one it
replaced.

Under load (section 7), a responder that receives an initiation whose `mac1`
verifies and whose `mac2` is not valid must reply with a cookie reply to the
source the initiation came from, and must do no Diffie-Hellman for that
initiation. When not under load, a responder does not check `mac2`.

In a cookie reply, `receiver_index` is the `sender_index` of the initiation
answered. `encrypted cookie` is XChaCha20-Poly1305 with key
`BLAKE2s("alloy/al/cookie/v1" || S)`, where `S` is the responder's static
public key, the `nonce` field as nonce, the 16-byte cookie as plaintext, and the
`mac1` of the initiation answered as associated data. The responder draws
`nonce` as 24 fresh random bytes from the `alloy-rt` randomness trait for every
reply. With 192 random bits, the chance that any two of 2^64 replies under one
static key share a nonce is below 2^-64 (I-07). A responder keeps no state per
cookie reply (I-06).

An initiator must drop a cookie reply whose `receiver_index` names no initiation
it has pending, or whose `encrypted cookie` does not decrypt with the `mac1` of
that initiation. Otherwise it keeps the cookie for that responder, in place of
any cookie it held for that responder, and uses it for `mac2` on every
initiation it sends to that responder for 120 seconds from receipt, then
discards it.

Cookie validity: a `cookie_secret` is current for 120 seconds and then accepted
as the previous one for 120 more, so a cookie stays valid for at least 120
seconds from its creation, as measured on the responder's clock. A responder
under load must accept a `mac2` computed with a cookie it sent in a cookie reply,
on every initiation from the same `src` that reaches it less than 120 seconds
after it computed that cookie. The initiator's 120 seconds start at receipt,
one network delay after creation, so this guarantee does not cover its whole
keep window: an initiation sent less than one round trip between the two nodes
before the window ends can reach the responder after the secret is retired. A
responder under load answers such an initiation with a fresh cookie reply, as
for any initiation without a valid `mac2`. This costs the initiator one round
trip and gives no party state or work beyond one cookie reply.

Key lifetimes: the `mac1` and cookie reply keys are derived from public keys
and live as long as the static key they come from. `cookie_secret` lives 120
seconds as above.

### 3.2 Handshake replay protection

Each node keeps a persistent 64-bit handshake counter that only grows. It reserves
blocks of 65,536 values with one synced write per block, so a crash skips values
and never repeats one. A responder stores the highest counter seen per initiator
static key and must drop an initiation whose counter is not higher. No wall clock
is involved. Across a process handover that table travels with the sessions, and
a process that resumes after a failed handover raises it to the counters its
successor accepted (3.9). The node channel takes its request ids from the same
counter (ARFC 0002 section 3.13), so each value is used once, by one initiation
or by one request, and initiation counters still only grow.

### 3.3 Session keys

After the response is processed, each side takes two values that a Noise
implementation exposes:

- `k1`, `k2`: the two 32-byte cipher keys that Noise `Split()` returns, `k1`
  being the one Noise assigns to the initiator's sending direction.
- `h`: the 32-byte handshake hash, as returned by Noise `GetHandshakeHash()`
  after the response.

It then derives with HKDF (RFC 5869) over HMAC-BLAKE2s, the HMAC and hash Noise
uses for this suite:

```
prk  = HKDF-Extract(salt = h, IKM = k1 || k2)
i2r  = HKDF-Expand(prk, "alloy/al/session-i2r/v1",  32)
r2i  = HKDF-Expand(prk, "alloy/al/session-r2i/v1",  32)
salt = HKDF-Expand(prk, "alloy/al/session-salt/v1", 14)
pi2r = HKDF-Expand(prk, "alloy/al/probe-i2r/v1",    32)
pr2i = HKDF-Expand(prk, "alloy/al/probe-r2i/v1",    32)
```

Labels are the ASCII bytes shown, with no terminator. The initiator's
`send_key` is `i2r` and its `recv_key` is `r2i`; the responder's are the
reverse. Both directions use the same `salt`. Likewise the initiator's
`probe_send_key` is `pi2r` and its `probe_recv_key` is `pr2i`, and the
responder's are the reverse; they seal only probe messages (3.6). The Noise
cipher states returned by `Split()` are never used to encrypt; transport uses
only `send_key`, `recv_key` and `salt` (3.4).

Erasure (`agent/eng/crypto.md` rule 3): once `send_key`, `recv_key`, `salt`,
`probe_send_key` and `probe_recv_key` exist, each side must erase every secret
handshake value that it or its Noise library holds: its ephemeral private key,
the chaining key, the handshake cipher key, `k1`, `k2`, the cipher states
`Split()` returned, and `prk`. A handshake that fails or is abandoned must erase
the same values when it ends. `send_key` and `recv_key` are erased when they are
discarded (below), and `probe_send_key` and `probe_recv_key` with them: each
probe key has the lifetime of the session key of the same direction, except that
after a rekey 3.6 keeps the previous session's probe keys longer, never past the
180 seconds below. A Noise library that
keeps a copy of any of these values that its caller cannot erase does not meet
this section.

Properties: Noise derives `k1` and `k2` from all four Diffie-Hellman results
and the pre-shared key, so the session keys are secret to the two nodes. `h`
covers the Noise message bytes, the responder static key and the pre-shared key,
and the protocol name binds the version, so the session keys are bound to both
static keys, both ephemeral keys, both payloads and the pre-shared key of this
handshake. They are not bound to the fields outside Noise (3.1): `type`,
`version`, `reserved`, `sender_index`, `receiver_index`, `mac1` and `mac2`. A
party on the path that rewrites those fields can make a handshake or a session
fail, as it can by dropping packets, but cannot change the keys, the identities
or the payloads. Once the values above are erased, a later compromise of either
static key or of the pre-shared key does not recover the session keys (forward
secrecy, section 8).

A session is rekeyed by a new handshake after 120 seconds of use or 2^30
transport packets on any queue, whichever comes first. A sender never uses a
transport counter of 2^31 or more (3.4), so every session stays below 2^31
transport packets per queue, well below the 2^32 that the 32-bit counter on the
wire distinguishes. Keys must be discarded 180 seconds after the handshake that
made them. The previous session's receive key stays valid for 10 seconds after a
rekey. Its probe keys follow 3.6. A rekey makes a new session, so state above AL
that lasts across rekeys is kept per peer mesh key, not per session, as the
node channel's is (ARFC 0002 section 3.13).

### 3.4 Transport (type 4)

| Field | Size | Notes |
|---|---|---|
| type | 1 | 4 |
| receiver_index | 4 | Names the session, the VNI and the sending queue at the receiver |
| counter | 4 | The low 32 bits of the packet's counter |
| ciphertext | n + 16 | AEAD over the inner frame, the 9 header bytes as associated data |

Type and version: type 4 is a transport packet under a version 1 session. A
session's version is the one its handshake carried (3.1), so a transport packet
does not repeat it. First byte values 0 and 7 to 255 are reserved: a later
version that changes this layout takes its type value from them. A version 1
receiver must drop, without reply, a message whose first byte is a reserved
value.

Fixed overhead per packet: 9 header + 16 tag = 25 bytes. With UDP (8 bytes) that
is 53 bytes over IPv4 (20) and 73 bytes over IPv6 (40). A relay envelope (3.7)
adds 32 more.

Blocks: at a node, a **block** is 64 index values `base + q` (mod 2^32), `q` from
0 to 63, held for one session and **bound** to one VNI. The value `base + q`
names that session, that VNI and the peer's sending queue `q`: it is the
`receiver_index` of every transport packet the peer seals on queue `q` for that
VNI in that session. Queues are numbered from 0 to `queues` - 1, where `queues`
is the count the peer sent in its handshake payload (3.1). A value `base + q`
with `q` at or above that count names nothing.

- A session's first block is the one its handshake reserved: its `base` is the
  `sender_index` the node sent in the initiation or response that made the
  session (3.1), and it is bound to VNI 0, the infrastructure network.
- Every other block of the session is bound to one VNI from 1 to 2^24 - 1 by a
  bind (below).
- Within a session a block's VNI never changes, and a VNI is bound to at most
  one block. A node holds at most 1,024 blocks per session, VNI 0 and up to 1,023
  others, so at most 65,536 indexes per session.
- A node holds at most 4 sessions with one peer, counting each initiation it has
  pending to that peer as one. Before it sends an initiation or a response that
  would make a fifth, it discards the oldest of the four (3.3), a pending
  initiation by abandoning it. A node therefore holds at most 4,096 blocks,
  262,144 indexes, per peer.
- A node frees a session's blocks when it discards the session (3.3), and reuses
  their values only after that. A packet that names a reused value is opened
  with the key of the new session, does not open, and is dropped.

Counters: each session has, in each direction, one 64-bit counter per sending
queue. It starts at 0 and grows by one with every transport packet sealed on
that queue in the session, whatever its VNI. The header carries its low 32 bits.
A sender never seals a transport packet with a counter of 2^31 or more: a queue
whose counter reaches 2^31 seals nothing more under that session, and its
packets are dropped and counted until a newer session exists. Rekeying at 2^30
packets (3.3) makes that session long before.

Nonce: `salt (14) || queue (2) || counter (8)`, where `queue` is the sending queue
that `receiver_index` names and `counter` is the full 64-bit counter. Each queue
has its own counter, so sending threads share nothing. Every block of a session
numbers the same queues and a queue's counter is shared by all the VNIs it
carries, so `queue || counter` never repeats under one `send_key`. Keys are per
session and per direction, so a nonce cannot repeat (I-07). Across a process
handover the counters travel with `send_key`, and a process that resumes after a
failed handover seals nothing under a session its successor used (3.9).

Replay window and counter reconstruction: the receiver keeps, per session and per
sending queue of the peer, a sliding replay window of 2,048 counters: the highest
counter `H` accepted on that queue in the session, and which of the 2,048
counters up to `H` were accepted. Every block of the session that names the queue
shares its window. The receiver reconstructs a packet's full counter `c` from the
32-bit value `w` in its header: with `e` equal to `H + 1`, or to 0 when nothing
has been accepted on the queue, `c` is the one integer such that
`c = w + k * 2^32` for some integer `k` and `e - 2^31 < c <= e + 2^31`. The
receiver must drop the packet if `c` is negative, is more than 2,047 below `H`,
or has been accepted already. Otherwise it opens the ciphertext with the nonce
built from `c`. Only a packet that opens changes the window: `c` is marked
accepted, and `H` becomes `c` when `c` is higher. The windows travel with
`recv_key` across a process handover, under the same rules (3.9).

The reconstruction is exact: every counter a sender uses is below 2^31, so the
interval holds the true counter of every packet that is not more than 2,047
below `H`. A packet reconstructed to any other counter does not open, because
the full counter is in the nonce, so the reconstruction can drop a packet but
never accept one. The reorder limit is therefore the window alone: a packet is
accepted however late it arrives, while its key is held and its counter is at
most 2,047 below the highest accepted on its queue.

Receive checks: before any cryptographic operation, a receiver must drop a
transport packet shorter than 26 bytes (the header, the tag and a 1-byte frame),
whose `receiver_index` is not an index of a session it holds (a value `base + q`
of one of its blocks, with `q` below the peer's `queues`), or that fails the
checks on `c` above.

Inner frame, after decryption: the plaintext is one frame of at least 1 byte.
Its **kind** is the high 4 bits of its first byte. An IP packet is recognized by
its own version field, and the other kinds use values that no IPv4 or IPv6
packet has there:

| Kind | Frame | Carried on |
|---|---|---|
| 0 | Keepalive: the single byte `0x00` | VNI 0 only |
| 1 | Control: `0x10`, `control type` (1), then the body of that type | VNI 0 only |
| 2 | Fragment (3.5): `0x20`, `frag_id` (4), `index` (1), `count` (1), then data | Any VNI |
| 4 | An IPv4 packet: the whole frame | Any VNI |
| 6 | An IPv6 packet: the whole frame | Any VNI |

"Carried on" is the VNI of the block that `receiver_index` names. After opening,
a receiver must drop a packet whose frame has a kind not in the table, has a kind
not carried on that VNI, or does not match the table: a keepalive that is not
exactly `0x00`, a control or fragment frame whose first byte is not exactly
`0x10` or `0x20`, a control frame that breaks the rules below, or a fragment
that breaks 3.5. An IPv4 or IPv6 frame is handed, with the VNI of its block, to
the virtual switch (ARFC 0009 section 3.2), or to the infrastructure network for
VNI 0. AL does not parse it further. A sender seals on a block of a VNI other
than 0 only IP packets and fragments of them: a packet handed to it for such a
VNI whose first 4 bits are not 4 or 6 is dropped and counted, so no guest packet
is ever read as a keepalive, a control frame or a fragment.

Control frames: the `control type` byte selects the body. Version 1 defines two
types, and a receiver must drop a control frame of any other type, or whose
length is not the one shown:

| Control type | Body | Frame length |
|---|---|---|
| 1, bind request | `vni` (3) | 5 |
| 2, bind | `vni` (3), `base` (4) | 9 |

Binding a VNI: VNI 0 is bound in every session without a bind, and each node
holds a binding for it in every session it holds: it seals its VNI 0 packets on
queue `q` with `receiver_index` `s + q`, where `s` is the `sender_index` the peer
sent in the handshake. For any other VNI `v`:

- A node that has a transport packet for `v` to a peer sends a bind request for
  `v` when it holds no binding for `v` in the session it would seal a VNI 0
  packet under (session choice, below). It sends it under that session, and
  again at most once per second while it has packets for `v` and still holds no
  binding for `v` there.
- A node that accepts a bind request for `v` under a session binds `v` in that
  session, to the block already bound to `v` if there is one and otherwise to a
  free block, and replies with a bind carrying `v` and that block's `base`. It
  seals the bind under the session the request came under, whatever the session
  choice below. It binds only when `v` is not 0, a VPC with VNI `v` has a port on
  it (ARFC 0009 section 3.3), and the session already binds `v` or holds fewer
  than 1,024 blocks with a free block to take. Otherwise it drops the request and
  counts it. It replies to every bind request it accepts, so a lost bind is
  recovered by the next request.
- A node that accepts a bind for `v` under a session, after it sent a bind
  request for `v` under that session, keeps it as its **binding** for `v` in
  that session when it holds none for `v` and fewer than 1,023 bindings for
  VNIs other than 0 there. From then on it seals that session's packets for `v`
  on queue `q` with `receiver_index` `base + q`. A bind that repeats the binding
  it holds changes nothing. Any other bind is dropped and counted.

Session choice: a node seals a transport packet for VNI `v` to a peer under the
newest session with that peer that is confirmed at the node (3.6) and in which it
holds a binding for `v`. For VNI 0 only, when no session with the peer is
confirmed, it seals under the newest session it holds with the peer. A packet for
which no session qualifies is held, up to 16 packets per VNI per peer and for at
most 1 second, and sealed once a binding exists; a packet it cannot hold, and a
packet held for 1 second, is dropped and counted. A node therefore goes on
sealing a VNI's packets under the previous session after a rekey until the new
session binds that VNI. An initiator seals a keepalive under a session as soon
as it makes it, so that the responder confirms the session at once (3.6).

### 3.5 Fragmentation

Payload budget: the largest inner frame that one transport packet carries on a
path is the path MTU (ARFC 0004 section 3.5) minus the IP and UDP headers (28
bytes over IPv4, 48 over IPv6), minus the 25 bytes of 3.4, minus 32 more when the
packet travels inside a relay envelope (3.7).

When an IPv4, IPv6 or control frame does not fit the path's payload budget, the
sender must split it into 2 to 16 fragments, each sent as its own transport
packet whose frame fits the budget. A keepalive is never split, and a fragment
is never split again. A frame that would need more than 16 fragments is dropped
and counted.

Fragment frame (kind 2), 7 bytes before its data:

| Field | Size | Content |
|---|---|---|
| kind | 1 | `0x20`: kind 2, low 4 bits zero |
| frag_id | 4 | Names the frame that was split |
| index | 1 | This fragment's position, from 0 to `count` - 1 |
| count | 1 | Number of fragments, 2 to 16 |
| data | rest | At least 1 byte |

The split frame is the data of its fragments concatenated in `index` order. A
sender seals every fragment of one frame under the same session with the same
`receiver_index`, so they share a VNI and a queue, and gives each frame it splits
on one `receiver_index` of a session a `frag_id` it has not used on that index in
that session.

A receiver reassembles by session, `receiver_index` and `frag_id`. It must drop
a fragment frame shorter than 8 bytes, whose first byte is not `0x20`, whose
`count` is outside 2 to 16, whose `index` is not below `count`, whose `count`
differs from that of a fragment it holds with the same `frag_id`, or whose
`index` it already holds. Once it holds all `count` fragments, it concatenates
their data and handles the result as a frame received on that block, except that
it must drop it unless its kind is 4 or 6, or 1 on a block of VNI 0. A reassembly
not complete 2 seconds after its first fragment arrived is abandoned and its
fragments are dropped. Reassembly memory is bounded as section 7 states.

A sender must never emit an underlay packet larger than the path MTU, and must
set the don't-fragment bit on IPv4 (I-10).

### 3.6 Probe (type 5)

Probe messages carry path discovery (ARFC 0004) on the same socket as data. They
travel inside a session. A node that is to start probing a peer with which it
holds no session first opens one (3.1) and sends its first probe message once
the session exists, which for a responder is once it has sent the response. As
ARFC 0004 section 3.2 step 4 defines, a **probe** carries a challenge and no
echo, and an **answer** carries an echo and may also carry a challenge.

Session choice: a session is **confirmed** at a node once the node has seen that
the peer holds it. At the initiator that is once it has processed the response.
At the responder it is once it has accepted, under that session, a transport
packet (one that authenticates and passes the replay check of 3.4) or a probe
message. One session is **newer** than another with the same peer when the node
made it later: an initiator makes a session when it processes the response, a
responder when it sends the response. Each node orders its own sessions; two
nodes that rekey at once may order the same two sessions differently, and every
rule of this section uses the order of the node applying it. A node seals every
probe message to a peer, answers included, under the
newest confirmed session whose `probe_send_key` it holds with that peer. Only
when it holds no confirmed session with the peer does it seal under the newest
session it holds with that peer.

At a rekey (3.3), the new session is therefore confirmed at the initiator when
the response arrives, and the responder keeps sealing under the previous session
until the initiator uses the new one. A lost or late response then leaves both
nodes on the previous session, which both still hold. To keep that working:

- A responder keeps the previous session's `probe_send_key` and probe send
  counter until a newer session with the same peer is confirmed, and no longer
  than the discard of the previous session's keys, 180 seconds after its
  handshake (3.3).
- A node keeps a session's `probe_recv_key` and probe window after a newer
  session with the same peer exists, until 10 seconds after it first accepts a
  probe message under a newer session from that peer, and no longer than that
  same discard. The 10 seconds let a probe message sealed under the older
  session that the underlay delivers late still be accepted. This overrides, for
  the probe keys only, the 10 seconds that 3.3 gives the previous session's
  receive key.
- Once a newer session is confirmed at a node, the node seals no further probe
  message under an older session with that peer.

Probe message (type 5), 88 bytes unless padded:

| Field | Size |
|---|---|
| type, version, reserved | 1, 1, 2 |
| receiver_index | 4 |
| counter | 8 |
| encrypted body | 56 + p + 16 |

`receiver_index` is the `sender_index` the receiver sent in the handshake that
made the session (3.1), the `base` of the session's VNI 0 block (3.4). A probe
message's `receiver_index` names a session only when it is that value: any other
index of the session names none, for probe messages. `counter` is the sender's probe counter in that session: it starts at
0 and grows by one with every probe message the sender seals in the session. It
is separate from the transport counters. A sender sends no probe message with a
`counter` of 2^48 or more in a session; it rekeys (3.3) first.

Body, before encryption:

| Field | Size | Content |
|---|---|---|
| flags | 1 | Bit 0: challenge present. Bit 1: echo present. Other bits zero |
| reserved | 3 | Zero |
| challenge | 16 | With bit 0: 16 fresh random bytes from the `alloy-rt` randomness trait. Otherwise zero |
| echo | 16 | With bit 1: the challenge answered, unchanged. Otherwise zero |
| observed family | 1 | 0 none, 4 IPv4, 6 IPv6 |
| reserved | 1 | Zero |
| observed port | 2 | |
| observed address | 16 | IPv4: the 4 address bytes, then 12 zero bytes |
| padding | p | Zero bytes |

Observed endpoint: an answer carries the source endpoint that the message it
answers arrived from, as the answering node saw it. Over UDP that is the source
address and port of the datagram, over the stream underlay (3.8) the remote
address and port of the stream. Address and port are in the byte order of the IP
and UDP headers. This is how an anchor reports a reflexive candidate (ARFC 0004
section 3.1.1). The observed family, port and address are zero in a probe, and
in an answer to a message that arrived inside a relay envelope (3.7).

Length: a message with bit 1 set is exactly 88 bytes, so `p` is 0. A probe is
88 bytes, or, when padded for path MTU discovery (ARFC 0004 section 3.5), long
enough that the underlay packet carrying it has the size under test: that size
minus the IP and UDP headers (28 bytes over IPv4, 48 over IPv6), minus 32 more
when the probe travels inside a relay envelope (3.7). `p` makes up the
difference. Padded probes are sent only over UDP and are at most 65,507 bytes.

Sealing: the encrypted body is XChaCha20-Poly1305 with the sender's
`probe_send_key` of the session (3.3), as nonce 16 zero bytes followed by
`counter` as 8 bytes little-endian, every byte of the message before the
encrypted body as associated data, and the body as plaintext. The receiver opens
it with its `probe_recv_key` of the session that `receiver_index` names. A probe
key seals in one direction of one session only and `counter` never repeats in
it, so a nonce is never used twice with a key (I-07). The two directions use
different keys, so a message reflected back to its sender does not open. No
Diffie-Hellman operation is done for a probe message, by its sender or its
receiver.

Replay window: for each session, the receiver keeps a sliding window of 2,048
probe counters, separate from the transport windows of 3.4: the highest
`counter` accepted in the session, and which of the 2,048 counters up to it were
accepted. A probe message passes the replay check when its `counter` has not
been accepted in that session and is at most 2,047 below the highest accepted. A
probe message is **accepted** when `receiver_index` names a session whose
`probe_recv_key` the receiver holds, its encrypted body opens with that key, it
passes the replay check, and it passes the fixed value checks below. Only an
accepted message changes the window. A receiver may run the replay check before
it opens the body. A node keeps a session's window exactly as long as it keeps
that session's `probe_recv_key` and erases the two together. A process that
hands a session over to its successor hands over the window with the
`probe_recv_key` and the probe send counter with the `probe_send_key`, and
follows the rules of 3.9 for the session afterwards.

Replay protection, as ARFC 0020 section 3.2 requires: no probe message is
accepted twice. While the receiver holds the key that opens a message, the window
rejects that message's `counter` once it has been accepted. Once the receiver
discards the key (3.3), or loses it in a crash or restart, nothing sealed under
it opens again, and every later session has new keys (3.3). Across a process
handover the window travels with the key, and a process that resumes after a
failed handover accepts nothing under a session its successor used (3.9). There
is no other state whose loss lets a message pass again. A message that was sent
and never
accepted, because the underlay dropped or delayed it, can still be accepted once,
while its key is held and its `counter` is within the window: that is a dropped
or reordered message, which ARFC 0020 section 2.1 allows the underlay attacker,
not a replay.

What an accepted message changes is defined by ARFC 0004 section 3.2: whether it
is answered, and which endpoint it verifies. A probe message is not a transport
packet. It carries no inner frame and it does not move the session's return path
(ARFC 0004 section 3.4). Because no message is accepted twice, a replayed probe
message draws no answer, verifies nothing, and adds no answer challenge, so
replays cannot displace the challenges a node puts in its answers (ARFC 0004
section 7). A replayed or unauthenticated probe message creates and refreshes no
state.

Fixed values: before any cryptographic operation, a receiver must drop a probe
message that is shorter than 88 bytes or longer than 65,507, whose `version` is
not 1, whose header `reserved` is not zero, or whose `receiver_index` names no
session whose `probe_recv_key` the receiver holds. After opening, it must drop a
message whose flags have neither bit 0 nor bit 1 set or have any other bit set;
whose reserved bytes, padding, or absent challenge or echo are not zero; whose
observed family is not 0, 4 or 6, or is 0 with a non-zero observed port or
address, or is 4 with a non-zero byte in observed address bytes 4 to 15; which
has bit 1 set and is not 88 bytes long; or which has bit 1 clear and an observed
family other than 0.

Cost: a probe message costs its sender one AEAD seal and its receiver at most one
AEAD open, the probe row of the table in section 6. Neither side does a
Diffie-Hellman operation or a synced write for it. The state is per session: two
32-byte probe keys, an 8-byte send counter, and a window of 8 bytes plus 2,048
bits, 336 bytes in all. A node holds it only for sessions it holds, so at most one
per session within the limit of ARFC 0004 section 7 (default 4,096), plus the
probe state of a previous session after each rekey, kept as the session choice
rules above say and never past 180 seconds after that session's handshake (3.3).

### 3.7 Relay envelope (type 6)

`type, version, flags (2), relay_handle (4), counter (8), tag (16)`, then an
opaque AL packet. The envelope is authenticated with the session key between the
node and the anchor, treating the opaque packet as associated data: it is
authenticated, not encrypted again. `relay_handle` names the far node and is
assigned by the anchor.

### 3.8 Stream framing

Over a stream underlay every AL message is prefixed with a 2-byte length. Nothing
else changes.

### 3.9 Process handover

A process that holds sessions may hand them over to a successor process on the
same node (ARFC 0017 section 3.5). This section keeps I-07 and the replay
protection of ARFC 0020 section 3.2, for handshake messages and for messages
sealed under session keys alike, across that handover, whether it succeeds or
fails.

State that travels: for every session handed over, the old process passes each
key with the state that keeps its nonces and its acceptances unique. With
`send_key` go the send counters of every queue (3.4). With `recv_key` go the
replay windows of every queue (3.4). With `probe_send_key` goes the probe send
counter (3.6). With `probe_recv_key` goes the probe window (3.6). Every other
counter or replay state kept for messages sealed or authenticated under a key of
the session, the relay envelope `counter` (3.7) and any replay state kept for
relay envelopes included, goes with the key it is used with. A key is never
passed without that state. With each session goes whether it is confirmed
(3.6), so the successor seals probe messages under the same session the old
process would have, and its blocks and bindings (3.4): the VNI each of its
blocks is bound to, and the `base` of every binding the old process holds in
it, so the successor reads every index as the old process did. With its sessions the old process passes its table of the
highest initiation counter seen per initiator static key (3.2).

Exclusive use: from the moment the old process passes its sessions, it seals no
message under any key of a session it passed and accepts none, and it processes
no handshake message (3.1), except as the failed handover rule below allows.

Consumed sessions: the successor must record a handed-over session as
**consumed** before it first does any of these:

- seals a message under a key of that session: a transport packet (3.4), a
  probe message (3.6) or a relay envelope (3.7);
- accepts a message under a key of that session;
- sends a handshake message (3.1) to the peer of that session.

Accepted initiations: before the successor stores an initiation's counter as the
highest it has seen from that initiator (3.2), it must record that initiator's
static key with that counter.

The record of consumed sessions and accepted initiations must be readable by the
old process after the successor has ended, whether it exited or crashed.
Recording a session consumed that the successor then does not use is allowed: it
only costs a new handshake. Recording an initiation counter that the successor
then does not store is allowed: it only makes the old process drop that
initiation as it drops a replay.

Failed handover: when a handover fails before the old process exits, the old
process serves on (ARFC 0017 section 6). It resumes only after the successor has
ended, by exiting, crashing or being stopped, so no session is ever used by two
processes at once. Before it seals or accepts any message after resuming, it
must:

1. Discard every consumed session: erase its keys, counters and windows as 3.3
   erases discarded keys, free its blocks (3.4), and never seal or accept under
   it again.
2. For every initiator in the record, raise the highest counter it holds for that
   initiator (3.2) to the highest counter recorded for it, when that is higher.
3. Reserve a new block of handshake counter values (3.2) and not use what was
   left of the block it held before the handover.

It resumes every session that is not consumed with its own state. Within 1
second of resuming, it sends an initiation (3.1) to every peer whose newest
session it discarded, whether or not it has a message for that peer, and retries
that handshake as it retries any other (3.1, section 9). Once the new session
exists, it seals a keepalive (3.4, kind 0) to the peer under it at once, so that
the peer sees the session used and confirms it (3.6). Messages the peer sends
under the discarded session are dropped as naming no session (section 6). After
a failed handover the drop therefore lasts at most 1 second plus the time that
handshake, its retries included, and that keepalive take, and it does not depend
on this node having a message for the peer.

Why this holds: the successor sealed and accepted only under consumed sessions,
so the counters and windows the old process keeps for every other session are
still current. A consumed session is never used by the old process again, so it
neither repeats a nonce the successor used with that session's keys (I-07) nor
accepts again a message the successor accepted (ARFC 0020 section 3.2). Step 2
puts the old process's table at or above every initiation counter the successor
stored, so it drops every initiation the successor accepted. The successor
reserves handshake counter values from the same persistent counter (3.2), so the
new block is above every value either process used. A successor that rekeyed or
replaced a session marked it consumed first, so the old process does not keep a
session the peer has moved off.

## 6. Failure behavior

Any message that fails to parse, authenticate, or pass the replay check must be
dropped silently, with a counter incremented and no reply (I-06).

Cost before authentication: a receiver must not spend more on a packet before
the packet is authenticated than the table below allows. "DH" is one X25519
operation. An initiation is authenticated when its encrypted payload decrypts
and its counter passes 3.2. A response is authenticated when its encrypted
payload decrypts. A probe message is authenticated when it is accepted (3.6).
Every other message is authenticated when its authentication tag verifies.

| Message | Allowed before it is authenticated |
|---|---|
| Initiation, not under load | One `mac1` check. Then two DH (`es`, `ss`) and the AEAD opens of the encrypted static and the encrypted payload |
| Initiation, under load | One `mac1` check, then at most two `mac2` checks, with the cookies computed from the current and the previous `cookie_secret`. If no `mac2` is valid: the AEAD seal of one cookie reply (3.1), and no DH. If one is valid: as when not under load |
| Response | One `mac1` check. Then, only if `receiver_index` names an initiation the receiver has pending: two DH (`ee`, `se`) and the AEAD open of the encrypted payload |
| Cookie reply | Only if `receiver_index` names an initiation the receiver has pending: the AEAD open of the encrypted cookie |
| Probe message | Only if it passes the checks that 3.6 makes before any cryptographic operation, which include that `receiver_index` names a session whose `probe_recv_key` the receiver holds: the replay check and one AEAD open of the encrypted body with that key. No DH, under load or not |
| Transport packet | Only if it passes the receive checks of 3.4, which include that `receiver_index` is an index of a session the receiver holds: the counter reconstruction, the replay check and one AEAD open with that session's `recv_key`. No DH, under load or not |
| Any other type | One MAC check, or one DH when not under load |

The responder computes the two DH that build the response (`ee`, `se`) only
after the initiation is authenticated, so they are not work before
authentication. A handshake message therefore costs its receiver at most two DH
before it is authenticated, an initiation under load without a valid `mac2`
costs none, and a completed handshake costs each side four DH in total, as
`IKpsk2` requires.

## 7. Limits

64 queues per session. 16 fragments per packet. 4 MiB of reassembly memory per
peer, 256 MiB per node, oldest dropped first. 65,535 bytes per stream frame. Under
load is defined as more than 1,000 pending handshakes.

Receiver indexes (3.4): blocks of 64 indexes, at most 1,024 blocks per session
(VNI 0 and up to 1,023 other VNIs), so 65,536 indexes per session. At most 4
sessions per peer, pending initiations included, so 4,096 blocks and 262,144
indexes per peer. Beyond the 1,024 blocks a bind request is dropped; a fifth
session discards the oldest. A sender holds at most 1,023 bindings per session
and, while it waits for one, at most 16 packets per VNI per peer for at most 1
second, dropping the rest. Transport counters stay below 2^31 per queue per
session.

## 8. Security

Provides confidentiality, integrity, replay protection, forward secrecy per
session, and initiator identity hiding. Does not hide traffic volume or timing.
Upholds I-06, I-07, I-10.

## 9. Open questions

- Hybrid post-quantum key exchange for the reserved flag. Research task to be cut
  in M10.
- When an initiator retries a handshake that got no response (3.1), and how many
  times. 3.9 bounds the drop after a failed handover by the time this takes.
