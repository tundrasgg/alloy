# ADR 0011: XChaCha20-Poly1305 with a Noise IK handshake

- Status: accepted
- Subject: mesh
- Specs: arfc-0003

## Context

The mesh needs an AEAD that is fast without hardware support and a handshake with identity hiding and one round trip.

## Decision

The handshake is Noise IK with a pre-shared key slot over X25519 and BLAKE2s. The AEAD everywhere is XChaCha20-Poly1305. Transport nonces are salt, queue, counter.

## Consequences

- Per-queue counters let sending threads share nothing.
- The 24-byte nonce leaves room for a per-session salt as defense in depth.

## Alternatives rejected

- AES-GCM: slow on machines without acceleration and a smaller nonce.
