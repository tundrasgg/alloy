# Engineering: cryptography

## Rules

1. Never implement a primitive. Use the crates below through `alloy-crypto`, which
   is the only crate that depends on them.
2. Never invent a construction in a build task. Constructions are specified in an
   ARFC first, and any task that touches one qualifies for the deep tier exception
   and always gets a review.
3. Compare secrets in constant time (`subtle`). Zeroize key material on drop
   (`zeroize`).
4. Randomness comes from the `alloy-rt` randomness trait. Production backs it with
   the operating system generator. Nothing else seeds anything.
5. Nonces are never chosen ad hoc. Each use has a specified layout and a specified
   reason it cannot repeat.
6. Keys have one purpose each. Derive separate keys with domain-separated labels
   of the form `alloy/<protocol>/<purpose>/v<n>`.
7. Every key has a stated lifetime and a rotation procedure before it is used.
8. Secrets never appear in logs, errors, metrics, debug output, or test fixtures
   committed to the tree.
9. Tests use fixed vectors from the primitive's specification where they exist.

## Primitives

| Purpose | Primitive | Crate |
|---|---|---|
| AEAD, mesh transport and at rest | XChaCha20-Poly1305 | `chacha20poly1305` |
| Key agreement | X25519 | `x25519-dalek` |
| Signatures, except the two uses in the next row | Ed25519 | `ed25519-dalek` |
| API CA key and API server certificate keys only | ECDSA P-256 with SHA-256, RFC 6979 nonces | The `rustls` crypto provider, once its ADR chooses one (R-0004) |
| Hash and KDF in the handshake | BLAKE2s, HKDF | `blake2`, `hkdf` |
| Content hashing | BLAKE3 | `blake3` |
| Request signing compatibility (SigV4) | HMAC-SHA-256 | `hmac`, `sha2` |
| Password hashing | Argon2id | `argon2` |
| TLS for API endpoints | TLS 1.3 | `rustls` |

ECDSA P-256 exists because browsers do not verify Ed25519 in a TLS chain
(ADR 0048). It signs nothing but certificates and TLS handshakes for API
endpoints, under the rules of `spec/arfc-0002-identity-and-join.md` section 2.
Every other key that signs is Ed25519: the cluster key, node keys, session
tokens, and anything new unless an ADR says otherwise.

Adding or changing a row is a behavior change to
`spec/arfc-0003-mesh-wire-protocol.md` or `spec/arfc-0002-identity-and-join.md`
and needs an ADR.

## Reserved

The handshake carries a version and a reserved hybrid key exchange slot so that a
post-quantum KEM can be added without a new message layout. Do not build it until
an ARFC specifies it.
