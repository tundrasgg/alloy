# ARFC 0003: AL, the mesh wire protocol

- Status: accepted
- Depends on: 0000
- Crates: alloy-wire, alloy-link, alloy-crypto
- Decisions: 0010, 0011, 0012, 0013, 0030

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

The pattern is Noise IK with a pre-shared key slot, instantiated as
`X25519`, `XChaCha20-Poly1305`, `BLAKE2s`. Where Noise calls for a 64-bit nonce
`n`, the 24-byte AEAD nonce is 16 zero bytes followed by `n`. The protocol name
string mixed into the initial hash is `alloy/al/handshake/v1`.

Initiation (type 1):

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
exchange and must be zero in version 1.

Response (type 2): `type, version, reserved, sender_index, receiver_index,
ephemeral (32), encrypted payload (4 + 16), mac1, mac2`. The payload is
`{queues: u16, flags: u16}`.

`mac1` is `BLAKE2s-128` keyed with `BLAKE2s("alloy/al/mac1/v1" || responder_static)`
over the message up to `mac1`. `mac2` is zero unless the responder has sent a
cookie. Under load a responder must reply with a cookie message (type 3) instead
of doing any Diffie-Hellman, and must then require a valid `mac2` bound to the
source address. Cookies rotate every 120 seconds.

### 3.2 Handshake replay protection

Each node keeps a persistent 64-bit handshake counter that only grows. It reserves
blocks of 65,536 values with one synced write per block, so a crash skips values
and never repeats one. A responder stores the highest counter seen per initiator
static key and must drop an initiation whose counter is not higher. No wall clock
is involved.

### 3.3 Session keys

After the handshake, each side derives with HKDF from the final chaining key:
`send_key`, `recv_key` (32 bytes each) and `salt` (14 bytes), with labels under
`alloy/al/session/v1`.

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

Probes carry path discovery (ARFC 0004) on the same socket as data. A probe is
`type, version, reserved (2), sender node_id (16), nonce (24), ciphertext`, sealed
with a key derived from the static-static shared secret under
`alloy/al/probe/v1`. Probes work before and without a session.

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
dropped silently, with a counter incremented and no reply (I-06). Before
authentication a receiver may do at most one MAC check per packet, or one DH per
packet when not under load.

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
