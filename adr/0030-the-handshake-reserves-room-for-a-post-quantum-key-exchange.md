# ADR 0030: The handshake reserves room for a post-quantum key exchange

- Status: accepted
- Subject: mesh
- Specs: arfc-0003

## Context

Recorded traffic can be decrypted later if the key exchange falls.

## Decision

The handshake has a version byte, a pre-shared key slot, and a reserved flag for a hybrid key exchange. Nothing post-quantum is built until an ARFC specifies it.

## Consequences

- A hybrid exchange can be added without a new message layout.
- A pre-shared key is available today for those who want it.

## Alternatives rejected

- Build a hybrid exchange now: choices in this area are still moving.
