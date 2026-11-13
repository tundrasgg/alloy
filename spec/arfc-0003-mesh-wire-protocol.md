# ARFC 0003: AL, the mesh wire protocol

- Status: accepted
- Depends on: 0000
- Crates: alloy-wire, alloy-link, alloy-crypto
- Decisions: 0010, 0011, 0012, 0013, 0030, 0043, 0046

## 1. Purpose

AL (Alloy Link) is the protocol between nodes. It gives authenticated, encrypted,
replay-protected datagrams tagged with a VPC, over UDP or a stream, with its own
fragmentation. This ARFC defines its bytes and state machines. Path selection is
ARFC 0004.

## 2. Model

A **session** exists between two node mesh keys. It is created by a two-message
handshake and holds one sending key and one receiving key per direction. A session
is independent of paths: any packet that authenticates belongs to the session
whatever underlay address it came from.

All integers are little endian. All messages start with a 1-byte `type` and a
1-byte `version`, currently `1`.

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

Indices: a node chooses its `sender_index` so that it is unique among its
pending handshakes and live sessions. The peer copies it, unchanged and opaque,
into the `receiver_index` of every response, cookie reply and transport packet
(3.4) it sends to that node.

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
is involved.

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
probe key has the lifetime of the session key of the same direction. A Noise library that
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

A session is rekeyed by a new handshake after 120 seconds of use or 2^48 packets
on any queue, whichever comes first. Keys must be discarded 180 seconds after the
handshake that made them. The previous session's receive key stays valid for 10
seconds after a rekey.

### 3.4 Transport (type 4)

| Field | Size | Notes |
|---|---|---|
| type, version | 1, 1 | |
| queue | 2 | Sender's queue id |
| receiver_index | 4 | |
| counter | 8 | Per session, per queue, starts at 0 |
| ciphertext | n + 16 | AEAD over the inner frame, header as associated data |

Nonce: `salt (14) || queue (2) || counter (8)`. Each queue has its own counter, so
sending threads share nothing. Because counters are per queue and keys are per
session, a nonce cannot repeat (I-07).

The receiver keeps a sliding replay window of 2,048 packets per session per queue
and must drop anything older or already seen.

Inner frame, after decryption:

| Field | Size | Notes |
|---|---|---|
| kind | 1 | 0 keepalive, 1 IPv4, 2 IPv6, 3 control, 4 fragment |
| vni | 3 | 0 means the infrastructure network |
| body | rest | For kind 4: `frag_id (4), index (1), count (1), inner kind (1), reserved (1)`, then data |

Fixed overhead per packet: 16 header + 4 inner + 16 tag = 36 bytes, plus UDP and
IP.

### 3.5 Fragmentation

When an inner packet does not fit the path's payload budget, the sender must split
it into 2 to 16 fragments, each sent as its own transport packet. Fragments are
reassembled before delivery. A reassembly is abandoned after 2 seconds. A sender
must never emit an underlay packet larger than the path MTU, and must set the
don't-fragment bit on IPv4 (I-10).

### 3.6 Probe (type 5)

Probe messages carry path discovery (ARFC 0004) on the same socket as data. They
travel inside a session: a node sends a probe message to a peer only under the
newest session it holds with that peer. A node that is to start probing a peer
with which it holds no session first opens one (3.1) and sends its first probe
message once the session exists, which for a responder is once it has sent the
response. As ARFC 0004 section 3.2 step 4 defines, a **probe** carries a
challenge and no echo, and an **answer** carries an echo and may also carry a
challenge.

Probe message (type 5), 88 bytes unless padded:

| Field | Size |
|---|---|
| type, version, reserved | 1, 1, 2 |
| receiver_index | 4 |
| counter | 8 |
| encrypted body | 56 + p + 16 |

`receiver_index` names the session at the receiver, as in a transport packet
(3.1, 3.4). `counter` is the sender's probe counter in that session: it starts at
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
hands a session over to its successor (ARFC 0017 section 3.5) hands over the
window with the key.

Replay protection, as ARFC 0020 section 3.2 requires: no probe message is
accepted twice. While the receiver holds the key that opens a message, the window
rejects that message's `counter` once it has been accepted. Once the receiver
discards the key (3.3), or loses it in a crash or restart, nothing sealed under
it opens again, and every later session has new keys (3.3). There is no other
state whose loss lets a message pass again. A message that was sent and never
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
per session within the limit of ARFC 0004 section 7 (default 4,096), plus a
previous session for the 10 seconds after each rekey (3.3).

### 3.7 Relay envelope (type 6)

`type, version, flags (2), relay_handle (4), counter (8), tag (16)`, then an
opaque AL packet. The envelope is authenticated with the session key between the
node and the anchor, treating the opaque packet as associated data: it is
authenticated, not encrypted again. `relay_handle` names the far node and is
assigned by the anchor.

### 3.8 Stream framing

Over a stream underlay every AL message is prefixed with a 2-byte length. Nothing
else changes.

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

## 8. Security

Provides confidentiality, integrity, replay protection, forward secrecy per
session, and initiator identity hiding. Does not hide traffic volume or timing.
Upholds I-06, I-07, I-10.

## 9. Open questions

- Hybrid post-quantum key exchange for the reserved flag. Research task to be cut
  in M10.
