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
   the operating system generator. Nothing else seeds anything. One exception,
   TLS (ADR 0042): under the `os` runtime the `rustls` provider (`aws-lc-rs`,
   ADR 0053) draws from its own generator, which `aws-lc` seeds from the
   operating system, exactly the values that live inside one TLS 1.3 handshake
   and that neither side keeps after it:
   - the ClientHello random and the ServerHello random, 32 bytes each;
   - the client's legacy session id, 32 bytes, and its extension order seed,
     16 bits;
   - the ephemeral secret of each key share either side makes, for X25519,
     secp256r1, or X25519MLKEM768, the ML-KEM decapsulation key and
     encapsulation randomness included.

   In rustls 0.23.46 these are `src/client/hs.rs` lines 119, 127, and 132,
   `src/server/hs.rs` line 513, and the `SupportedKxGroup` implementations of
   `src/crypto/aws_lc_rs`. Nothing else comes from it: every key Alloy keeps,
   every nonce Alloy chooses, and every secret Alloy sends comes from the trait,
   the API CA and API server keys included. Session tickets, resumption, early
   data, Encrypted Client Hello and its GREASE are off, since each would draw
   more. Under the `mem` `net` implementation, where no byte leaves the
   process, no TLS runs, so a simulated trace holds no byte from that generator
   (I-20, `spec/arfc-0008-native-api.md` clause 3.8.1). Under any other `net`
   implementation the API and `stream` run on TLS as under `os`, or are not
   served: cleartext is allowed only where no byte leaves the process.
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
| Signatures, except the three keys in the next row | Ed25519 | `ed25519-dalek` |
| API CA key, API server certificate keys, ACME account key only | ECDSA P-256 with SHA-256, RFC 6979 section 3.2 nonces | `p256`, `ecdsa`, `rfc6979` for keys and signatures; `rcgen`, default features off, for certificates and CSRs (ADR 0053) |
| Hash and KDF in the handshake | BLAKE2s, HKDF | `blake2`, `hkdf` |
| Content hashing | BLAKE3 | `blake3` |
| Request signing compatibility (SigV4) | HMAC-SHA-256 | `hmac`, `sha2` |
| Password hashing | Argon2id | `argon2` |
| TLS for API endpoints | TLS 1.3 | `rustls` with the `aws-lc-rs` provider (ADR 0053) |

ECDSA P-256 exists because browsers do not verify Ed25519 in a TLS chain
(ADR 0048) and because `ES256` is the only signature algorithm every ACME
server must implement (ADR 0053). Three keys are P-256, and each signs only
these messages:

- API CA key: its own certificate and the certificates it issues.
- API server certificate key: the TLS 1.3 `CertificateVerify`, and the PKCS#10
  CSR (RFC 2986) of an ACME order (RFC 8555 section 7.4). The key of the
  self-signed `stream` certificate of an anchor without `control` is one of
  these, and also signs that certificate
  (`spec/arfc-0004-path-manager.md` section 3.6).
- ACME account key: the JWS of every ACME request, as `ES256` (RFC 7518
  section 3.4).

Every one of these signatures follows `spec/arfc-0002-identity-and-join.md`
section 2: the nonce comes from RFC 6979 section 3.2 with SHA-256, so signing
uses no random source and the same key signing the same message yields the same
signature, and the key is generated from the `alloy-rt` randomness trait. The
plain `Signer` of `p256` and `ecdsa` meets RFC 6979 and is the only signer used
for these keys. The signers of every `rustls` provider and the randomized
signers of `ecdsa` draw the nonce from a generator (R-0008) and never sign
with an Alloy key. Alloy's P-256 keys reach `rustls` only as
`rustls::sign::SigningKey` values, so the provider only verifies P-256.

Every other key that signs is Ed25519: the cluster key, node keys, session
tokens, and anything new unless an ADR says otherwise.

Adding or changing a row is a behavior change to
`spec/arfc-0003-mesh-wire-protocol.md` or `spec/arfc-0002-identity-and-join.md`
and needs an ADR.

## Reserved

The handshake carries a version and a reserved hybrid key exchange slot so that a
post-quantum KEM can be added without a new message layout. Do not build it until
an ARFC specifies it.
