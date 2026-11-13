# ARFC 0008: Native API

- Status: accepted
- Depends on: 0006, 0007
- Crates: alloy-proto, alloy-api
- Decisions: 0026, 0042, 0048, 0053

## 1. Purpose

One API is the source of truth for everything Alloy does. Every other surface is a
client of it.

## 2. Model

The API is defined in Protocol Buffers under `proto/alloy/<service>/<version>/`.
It is served as gRPC (section 3.12). A JSON over HTTP form generated from the
same definitions, with an OpenAPI document at `/openapi.json`, and a form
browsers can call, are added by the spec task of the console (ARFC 0019 section
3.2). Until that task, no other form is served, and a request that is not gRPC
fails as section 3.12 says.

Services: `cluster`, `iam`, `compute`, `network`, `block`, `object`, `registry`,
`audit`, `metering`. Each service has resource kinds, and each kind has the
standard methods `Create`, `Get`, `List`, `Update`, `Delete`, `Watch`, plus custom
methods named `Verb` + `Kind`.

Names follow `product/principles.md` rules A1 to A3. A kind is singular
PascalCase in Protocol Buffers (`Node`) and its collection is plural, lower
case, and hyphenated (`nodes`). A resource's full name is its parent's full
name, then the collection and the resource's name: a cluster-scoped resource
has no parent, so a node's full name is `nodes/<name>`. Errors, audit records,
and `--output name` use the full name.

## 3. Behavior

3.1 Versions are `v1alpha1`, `v1beta1`, `v1`. Alpha may change freely. Beta
changes only compatibly within a minor release. A stable version never breaks: no
field is removed, renumbered, or changes meaning.

3.2 Every mutating method accepts an `idempotency_key`. Repeating a call with the
same key within 24 hours must return the first result and must not act twice.

3.3 Operations that do not finish within the call return an `Operation` resource
with `done`, `error`, `result`, and progress. Operations are kept for 7 days.

3.4 `List` paginates with an opaque `page_token`, default page size 100, maximum
1,000, and accepts `filter` (a label and field expression) and `order_by`. A
`page_size` of 0 means 100, and one above 1,000 is reduced to 1,000, not
refused. The response carries `next_page_token`, empty on the last page and
only there. A `page_token` sent with a `filter` or `order_by` other than those
of the call that returned it fails with `InvalidArgument`. The default order is
by name, ascending by byte value. Until the filter grammar is specified (rule
A9 of `product/principles.md`), a List accepts only an empty `filter` and an
`order_by` that is empty or `name`, and fails with `InvalidArgument` naming the
field otherwise.

3.5 `Update` takes a field mask and the `generation` read. A stale generation
fails with `Conflict`.

3.6 Errors carry a stable `code`, a human `message`, the `resource` concerned, and
`details`. Codes are defined once, in `proto/alloy/common/errors.proto`, and are
never reused. A message says what happened, why, and what to do next. Each code
has one gRPC status, one HTTP status for the later JSON form, and one CLI exit
code (ARFC 0019 clause 3.1.7):

| Code | gRPC status | HTTP | CLI exit |
|---|---|---|---|
| `InvalidArgument` | `INVALID_ARGUMENT` (3) | 400 | 2 |
| `Unauthenticated` | `UNAUTHENTICATED` (16) | 401 | 4 |
| `PermissionDenied` | `PERMISSION_DENIED` (7) | 403 | 4 |
| `NotFound` | `NOT_FOUND` (5) | 404 | 3 |
| `Conflict` | `ABORTED` (10) | 409 | 5 |
| `QuotaExceeded` | `RESOURCE_EXHAUSTED` (8) | 403 | 1 |
| `RateLimited` | `RESOURCE_EXHAUSTED` (8) | 429 | 6 |
| `Compacted` | `OUT_OF_RANGE` (11) | 410 | 1 |
| `Unavailable` | `UNAVAILABLE` (14) | 503 | 6 |
| `Internal` | `INTERNAL` (13) | 500 | 1 |

On gRPC the error travels as the call's status, with the message as
`grpc-message`, and with the trailer `alloy-error-bin` holding the encoded
`alloy.common.v1alpha1.Error`: `code` (enum), `message`, `resource` (full
name), `field_violations` (each a field path and the rule it breaks, every
invalid field listed, rule A12), `retry_delay_duration` (for `RateLimited` and
`Unavailable`), and `next_step` (an API method and its CLI form, rule A11).
Codes that later tasks add join the table.

3.7 Authentication: a session token, or SigV4 with an access key. Every call is
authorized (ARFC 0007) and, when mutating, audited (ARFC 0018).

3.7.1 SigV4 on gRPC follows AWS Signature Version 4
(https://docs.aws.amazon.com/IAM/latest/UserGuide/reference_sigv-create-signed-request.html)
with these values. The HTTP method is `POST` and the canonical URI is the gRPC
path, `/<package>.<Service>/<Method>`. The canonical query string is empty. The
signed headers are exactly `content-type`, `host`, `x-amz-content-sha256`, and
`x-amz-date`, where `host` takes the value of the HTTP/2 `:authority`
pseudo-header. `x-amz-content-sha256` is the lower case hexadecimal SHA-256 of
the whole HTTP/2 request body, the gRPC length prefix included (section 3.12),
and is the hashed payload. The credential scope is
`<YYYYMMDD>/global/alloy/aws4_request`. The `authorization` header is
`AWS4-HMAC-SHA256 Credential=<access key id>/<scope>, SignedHeaders=<list>,
Signature=<hex>`. The server must refuse with `Unauthenticated`, before any
other check and the same for each of these causes, a call whose signature does
not verify, whose body does not hash to `x-amz-content-sha256`, whose
`x-amz-date` is more than 5 minutes from the server's clock, or whose access key
is unknown or revoked. A session token travels as the header `authorization:
Bearer <token>` (ARFC 0007 section 2.3). A call with neither, other than
`ExchangeLoginCode` (ARFC 0002 section 3.7), fails with `Unauthenticated`.

3.7.2 The local admin socket (ARFC 0007 clause 3.10.2) serves the same gRPC
methods over HTTP/2 without TLS, by prior knowledge (RFC 9113 section 3.3), and
takes no authentication header: each call is a local admin call.

3.8 The API is served on TLS 1.3 only, on the `api_port` of every control node
(ARFC 0002 section 2), TCP 443 unless `init` or `join` was given `--api-port`,
at `https://api.<cluster-domain>` or by address, on every local address, IPv4
and IPv6. Clients reach it over the underlay, never over the mesh. The same
port carries the `stream` underlay when the node holds `anchor` (ARFC 0004
section 3.6), so that both pass networks that allow only TCP 443. The server
tells the two apart by ALPN (RFC 7301), reading the ClientHello before it
chooses a configuration (`rustls::server::Acceptor`, whose `ClientHello::alpn`
returns the client's protocol ids, rustls 0.23.46). It selects `h2` for the API
when the node holds `control` and the client offers `h2`, and `alloy-al/1` for
`stream` when the node holds `anchor` and the client offers it, preferring `h2`
when a client offers both. Any other ClientHello, one without ALPN included, is
refused with the alert `no_application_protocol` (RFC 7301 section 3.2). The
server sends no session ticket and keeps no resumption state, and the client
offers neither resumption nor early data. A client verifies the presented chain
to the API CA certificate whose fingerprint its context pins (ARFC 0002 section
3.7, ARFC 0019 section 2) and requires the address it connected to among the
leaf certificate's `iPAddress` names, or, for an ACME certificate, verifies it
as a web PKI client does. The certificate comes from ACME when
the cluster has a public name, otherwise from the API CA whose fingerprint the CLI
pins at first login. Whichever source issues it, the API server certificate's key
is ECDSA P-256, generated and encoded as ARFC 0002 section 2 says, and the server
signs its TLS 1.3 `CertificateVerify` with the scheme `ecdsa_secp256r1_sha256`
(RFC 8446 section 4.2.3) (ADR 0048). A certificate from the API CA is signed by
the API CA with `ecdsa-with-SHA256`, and the server presents it followed by the
API CA certificate, so every key and every signature in that chain is ECDSA
P-256 and the browsers of ARFC 0019 clause 3.2.4 can verify it. An ACME
certificate's own signature and the rest of its chain are the issuer's, and only
its key is Alloy's choice.

3.8.1 TLS randomness. Under the `os` runtime the `rustls` provider (`aws-lc-rs`,
ADR 0053) draws from its own generator the values that live inside one TLS 1.3
handshake and that neither side keeps after it, and nothing else: the
ClientHello and ServerHello randoms, the client's legacy session id and
extension order seed, and the ephemeral secrets of the key shares, as
`agent/eng/crypto.md` rule 4 lists them and allows for TLS alone (ADR 0042).
Session tickets, resumption, early data, and Encrypted Client Hello are off, on
the server and on every client. Under the `mem` implementation of the `net`
seam (ARFC 0005 section 2.3), where no byte leaves the process, no TLS runs:
the API server, its clients, the `stream` underlay, and the ACME client
exchange in the clear the bytes they would carry inside TLS, and the server
tells the API from `stream` by the HTTP/2 connection preface (RFC 9113 section
3.4), which an API client sends first and `stream` never does. So no byte of a
simulated run comes from a generator outside `alloy-rt` (I-20). Under any other
`net` implementation the API and `stream` run on TLS as under `os`, or are not
served, so no API call and no `stream` byte crosses a network in the clear.
Certificate verification and ALPN are tested under `os`.

3.9 Rate limits apply per principal, default 100 requests per second with a burst
of 200, and return `RateLimited` with a retry delay.

3.10 `alloy-proto` generates the Rust client. The CLI and console use only
generated clients. The Rust server and client are built on `tonic` over
`prost`, the standing Protocol Buffers choice (`agent/eng/deps.md`, R-0007
option B). The console's client is chosen by R-0007 and specified with the
console.

3.11 Every field, method, and error code has documentation in the definition. CI
fails on an undocumented item and on any incompatible change to a beta or stable
version.

3.12 Wire. Each service `<service>` is the Protocol Buffers service
`<Service>Service`, its name in PascalCase, of package
`alloy.<service>.<version>` (`alloy.cluster.v1alpha1.ClusterService`), and each
method is a gRPC method over HTTP/2 as the gRPC protocol specifies
(https://github.com/grpc/grpc/blob/master/doc/PROTOCOL-HTTP2.md): path
`/<package>.<Service>/<Method>`, `content-type` `application/grpc`, and each
message prefixed by 1 byte of compression flag, always 0, and its length in 4
bytes, big endian. No message is compressed. Methods are unary or, for `Watch`,
server streaming. No method streams from the client. A request that is not
HTTP/2, or names a path that is no method, fails with gRPC status
`UNIMPLEMENTED` (12) and no Alloy code. Every request and response is at most 4
MiB (section 7), and a larger one fails with `InvalidArgument`.

3.13 The `cluster` service at version `v1alpha1` holds the kind `Node`, one per
node, whose fields are the node record's (ARFC 0002 section 2) as `name`, `uid`,
`roles`, `zone`, `advertise`, `create_time`, and `generation`, and whose status
ARFC 0006 defines. Its methods are `GetNode`, `ListNodes`, `UpdateNode`,
`ApproveNode`, and `RejectNode`, and the join token methods of ARFC 0002 section
3.2. The `iam` service at `v1alpha1` holds `IssueLoginCode` and
`ExchangeLoginCode` (ARFC 0002 section 3.7). These are the methods the CLI of
ARFC 0019 clauses 3.1.11, 3.1.12, and 3.1.14 calls.

## 6. Failure behavior

Without control plane quorum, mutating calls fail with `Unavailable`, and reads
succeed when the caller allows stale reads. A call that times out at the client
may have taken effect: clients retry with the same idempotency key.

## 7. Limits

4 MiB per request and per response, larger payloads go through the object and
registry data paths. 1,000 items per page. 256 characters per idempotency key. 64
labels per resource, keys and values up to 63 and 255 characters.

## 8. Security

The API is reached only over TLS 1.3 to a certificate that chains to the pinned
API CA or to the web PKI (section 3.8), so an underlay attacker reads and
changes nothing. Only under the `mem` `net` implementation, inside one process,
does it run without TLS (clause 3.8.1). A SigV4 signature covers the method, the server name, the
body, and the time, so a signed call cannot be moved to another method or given
another body, and is refused 5 minutes after it was made. The local admin socket
carries no credential and is reached only by user id 0 (ARFC 0007 clause
3.10.2). Port sharing by ALPN gives a `stream` client no API access and an API
client no AL session: each protocol authenticates on its own. The provider's
own randomness reaches only the TLS handshake values of clause 3.8.1, which
neither side keeps, never a key or nonce Alloy keeps (ADR 0042). Upholds I-02, I-15, I-20, I-22.
