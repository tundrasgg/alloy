# ADR 0043: The AL handshake is fixed byte for byte on Noise IKpsk2

- Status: accepted
- Subject: mesh
- Specs: arfc-0003

## Context

ARFC 0003 section 3.3 derived session keys from the final Noise chaining key. Noise `Split()` returns only cipher keys, and `snow`, the library R-0004 recommends, exposes the split keys and the handshake hash but never the chaining key. Section 3.1 also left open the byte order of the nonce counter, the position of the pre-shared key, the response field sizes, the reserved bytes, and the cookie reply and `mac2`. Two implementations could not interoperate, and the first could not be built on a stock Noise library.

## Decision

- The handshake is Noise `IKpsk2` with an empty prologue. The pre-shared key is 32 bytes, all zero when none is configured.
- The handshake nonce counter `n` is 8 bytes little-endian after 16 zero bytes.
- Session keys are `HKDF-Extract(salt = h, IKM = k1 || k2)` over HMAC-BLAKE2s, expanded with the labels `alloy/al/session-i2r/v1`, `alloy/al/session-r2i/v1` (32 bytes each) and `alloy/al/session-salt/v1` (14 bytes), where `k1`, `k2` are the `Split()` keys and `h` is the handshake hash.
- Once the session keys exist, the ephemeral private key, chaining key, handshake cipher key, split keys and `prk` are erased, including any copy a Noise library holds.
- The header fields outside Noise (`type`, `version`, `reserved`, the indices, `mac1`, `mac2`) are not bound into the handshake. Reserved bytes and flags are zero in version 1 and a receiver drops a message that breaks that.
- `mac1` is keyed from the static public key of the message's recipient. The cookie is `MAC(cookie_secret, src)` with a random `cookie_secret` replaced every 120 seconds. The cookie reply (type 3, 64 bytes) carries it under XChaCha20-Poly1305 with a key from the responder static key, a random 24-byte nonce, and the initiation's `mac1` as associated data.

## Consequences

- Sections 3.1 and 3.3 run on any Noise library that exposes the split keys, the handshake hash, setting the pre-shared key between messages, and erasure of its state. `snow` 0.10.0 erases nothing, so the task that picks the crate must close that gap.
- Session keys are bound to both static keys, both ephemeral keys, both payloads and the pre-shared key, not to the header fields. Rewriting a header field can only make a handshake or session fail.
- Forward secrecy rests on a stated, testable erasure rule rather than on library behavior.
- A per-pair pre-shared key is chosen after the responder learns the initiator static key, with no lookup before the first message.
- A responder under load answers with a stateless cookie reply and does no Diffie-Hellman. Every handshake message has a fixed length.
- The Noise cipher states from `Split()` are discarded unused. Transport keys and nonces stay as in section 3.4.

## Alternatives rejected

- Keep the chaining key derivation: needs a forked library or a hand-built Noise state machine, which `agent/eng/crypto.md` rules 1 and 2 push against.
- Use the `Split()` keys directly as transport keys: gives no 14-byte salt and no domain-separated labels (crypto rule 6).
- `psk0` or `psk1`: the responder would need the pre-shared key before it can know which initiator sent the first message.
- Bind the header fields through the prologue: the prologue is fixed before the first message, so the response header cannot enter it, and binding only the initiation header protects nothing an on-path party cannot already break by dropping packets.
- A counter as the cookie reply nonce: needs persisted state per responder, where 192 random bits keep the reply stateless with a stated collision bound.
