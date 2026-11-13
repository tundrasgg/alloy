# ADR 0048: The API CA and API server certificates use ECDSA P-256

- Status: accepted
- Subject: api
- Specs: arfc-0002, arfc-0008, arfc-0019

## Context

ARFC 0002 made the API CA key Ed25519. Chromium and Firefox neither offer Ed25519 in TLS nor validate an Ed25519-signed certificate, so the console of ARFC 0019 clause 3.2.4 would not load on a cluster without a public name (R-0006). Apple documents only RSA and EC keys. The CLI pins the API CA by a fingerprint in the login code and verifies either type.

## Decision

- The API CA key and every API server certificate key, from the API CA or from ACME, are ECDSA P-256. Every signature the API CA makes is `ecdsa-with-SHA256`, so a chain from the API CA holds no other key type or signature algorithm.
- Every ECDSA P-256 signature takes its nonce by RFC 6979 section 3.2 with SHA-256. Signing uses no random source.
- `api_ca_fp` is the first 16 bytes of `BLAKE3` over the DER SubjectPublicKeyInfo of the API CA certificate, with the point uncompressed (91 bytes). The login code layout and length do not change.
- Every other signing key stays Ed25519. `agent/eng/crypto.md` scopes the new primitive to these two uses.

## Consequences

- The console can load on a private cluster once the user trusts the API CA in the browser.
- `alloy-crypto` gains ECDSA P-256 key generation and RFC 6979 signing; tests use the RFC 6979 P-256 vectors.
- The `rustls` provider ADR must confirm RFC 6979 signing for P-256, or name what supplies it.
- The fingerprint covers the algorithm identifiers, so it cannot match a key of another type.

## Alternatives rejected

- Ed25519 CA with an ECDSA leaf: the leaf would carry an Ed25519 signature that Chromium cannot validate.
- Drop browser support on private clusters, or require ACME for the console: breaks ARFC 0019 clause 3.2.4.
- RSA-2048: larger and slower keys, and a second new primitive with no gain over P-256 in the browsers read.
- Random ECDSA nonces: a weak generator leaks the key, and signing would draw on a source outside `alloy-rt`.
- Fingerprint over the raw 65-byte point: loses the algorithm binding the SubjectPublicKeyInfo gives.
