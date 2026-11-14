# ARFC 0008: Native API

- Status: accepted
- Depends on: 0006, 0007
- Crates: alloy-proto, alloy-api
- Decisions: 0026, 0042, 0048, 0053, 0070

## 1. Purpose

One API is the source of truth for everything Alloy does. Every other surface is a
client of it.

## 2. Model

The API is defined in Protocol Buffers under `proto/alloy/<service>/<version>/`.
Every control node serves it on one port (section 3.8) in two forms, both
generated from the same definitions: gRPC (clause 3.12), which the CLI calls,
and the Connect form (clause 3.14), which the console, `curl`, and any other
HTTP client call. The Connect form's JSON encoding is the API's JSON over HTTP
form, and its OpenAPI document is served at `/openapi.json` (clause 3.18). The
same port serves the console's files (ARFC 0019 clause 3.2.6). Clause 3.12.1
says how the server tells these requests apart.

Services: `cluster`, `iam`, `operation`, `compute`, `network`, `block`,
`object`, `registry`, `audit`, `metering`. Each service has resource kinds, and each kind has the
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
with `done`, `error`, `result`, and progress. Operations are kept for 7 days
after they are done, then deleted.

3.3.1 An `Operation` has these fields, and its progress follows rule A16 of
`product/principles.md`:

| Field | Content |
|---|---|
| `name` | Full name, under the narrowest scope that holds its `target`: `orgs/<org>/projects/<project>/operations/<id>` when the target is a resource in that project; `orgs/<org>/operations/<id>` when it is a resource of that org outside every project, such as a project itself, a user, an access key, or a policy; and `operations/<id>` only when it is a cluster resource (ARFC 0007 section 2.5), such as a node, a pool, or an org itself. `<id>` is generated (rule A3) |
| `uid`, `create_time`, `update_time` | As for every resource (rule A4) |
| `method` | The method that started it, `<package>.<Service>/<Method>` |
| `target` | The full name of the resource it acts on |
| `principal` | The caller that started it, as its audit record names it (ARFC 0018) |
| `done` | Set once, when it ends. No field changes after that |
| `error` | An `alloy.common.v1alpha1.Error` (clause 3.6) when it ended in failure |
| `result` | The method's response, as a `google.protobuf.Any`, when it ended in success |
| `stage` | The current stage as one sentence, as the CLI prints it. Empty once done |
| `stage_start_time` | When the current stage began |
| `stages_done` | Each finished stage in order, with its sentence, `start_time`, and `end_time` |
| `work_done`, `work_total` | The amount of the current stage done and its total, in the unit its sentence names. Both 0 when the size of the work is not known |

3.3.2 The `operation` service at `v1alpha1` holds `GetOperation`,
`ListOperations`, and `WatchOperations` (clause 3.15). `ListOperations` and
`WatchOperations` take `parent`: a project's full name for that project's
operations, an org's full name for the operations named
`orgs/<org>/operations/<id>`, or empty for the operations named
`operations/<id>`. Reading a project's operations is authorized as
`operation:GetOperation` and `operation:ListOperations` on that project, which
every built-in org and project role allows. Reading an org's operations is
authorized as the same actions on that org, which every built-in org role
allows. The cluster roles grant no access to an org's or a project's
operations (ARFC 0007 clause 3.3). Reading the operations named
`operations/<id>` is allowed by the cluster roles `cloud-admin` and
`cloud-viewer` and by no other built-in role. No operation's `result` holds a
secret (I-22): a method whose response carries one finishes within the call
and returns no operation. A `parent` that is another org, or a project of
another org, answers as ARFC 0007 clause 3.2 says.

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
has one gRPC status, one HTTP status on the Connect form (clause 3.14.4), which
is the status the Connect protocol maps that gRPC status to
(https://connectrpc.com/docs/protocol/, Error Codes), and one CLI exit code
(ARFC 0019 clause 3.1.7):

| Code | gRPC status | HTTP | CLI exit |
|---|---|---|---|
| `InvalidArgument` | `INVALID_ARGUMENT` (3) | 400 | 2 |
| `Unauthenticated` | `UNAUTHENTICATED` (16) | 401 | 4 |
| `PermissionDenied` | `PERMISSION_DENIED` (7) | 403 | 4 |
| `NotFound` | `NOT_FOUND` (5) | 404 | 3 |
| `Conflict` | `ABORTED` (10) | 409 | 5 |
| `QuotaExceeded` | `RESOURCE_EXHAUSTED` (8) | 429 | 1 |
| `RateLimited` | `RESOURCE_EXHAUSTED` (8) | 429 | 6 |
| `Compacted` | `OUT_OF_RANGE` (11) | 400 | 1 |
| `Unavailable` | `UNAVAILABLE` (14) | 503 | 6 |
| `Internal` | `INTERNAL` (13) | 500 | 1 |

On gRPC the error travels as the call's status, with the message as
`grpc-message`, and with the trailer `alloy-error-bin` holding the encoded
`alloy.common.v1alpha1.Error`: `code` (enum), `message`, `resource` (full
name), `field_violations` (each a field path and the rule it breaks, every
invalid field listed, rule A12), `retry_delay_duration` (for `RateLimited` and
`Unavailable`), and `next_step` (an API method and its CLI form, rule A11).
On the Connect form the same `Error` travels as clause 3.14.4 says. Codes that
later tasks add join the table.

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
Bearer <token>` (ARFC 0007 section 2.3), or, on the Connect form only, as the
cookie of clause 3.14.6. A call with no credential, other than
`ExchangeLoginCode` (ARFC 0002 section 3.7) and `CreateSessionToken` with a
login code (ARFC 0007 clause 3.12.1), fails with `Unauthenticated`. Clause
3.14.5 says how SigV4 applies to the Connect form.

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
option B). The console's client is generated by `protoc-gen-es` (protobuf-es)
for the Connect-ES runtime and calls the Connect form (clause 3.14) (ADR 0070,
R-0007). `cargo xtask` generates both clients from the files under `proto/`,
and no generated file is edited by hand.

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
HTTP/2 fails with gRPC status `UNIMPLEMENTED` (12) and no Alloy code. Every
request and response is at most 4 MiB (section 7), and a larger one fails with
`InvalidArgument`.

3.12.1 Routing. The server sends each HTTP/2 request on the API port to the
first row of this table that matches it:

| Request | Served by |
|---|---|
| `content-type` is `application/grpc` or starts with `application/grpc+` | gRPC, this clause. A path that is no method fails with `UNIMPLEMENTED` (12) and no Alloy code |
| The path starts with `/alloy.` | The Connect form, clause 3.14 |
| `GET` or `HEAD` of `/openapi.json` | The OpenAPI document, clause 3.18 |
| Any other `GET` or `HEAD` | The console's files, ARFC 0019 clause 3.2.6 |
| Anything else | HTTP status 405 and an empty body |

Every response on the API port, of every row, carries the header
`alloy-release`, whose value is the running release of the node that answered
(ARFC 0017 clause 3.11).

3.13 The `cluster` service at version `v1alpha1` holds the kind `Node`, one per
node, whose fields are the node record's (ARFC 0002 section 2) as `name`, `uid`,
`roles`, `zone`, `advertise`, `create_time`, and `generation`, and whose status
ARFC 0006 defines. Its methods are `GetNode`, `ListNodes`, `WatchNodes`,
`UpdateNode`, `ApproveNode`, and `RejectNode`, the join token methods of ARFC
0002 section 3.2 with `WatchJoinTokens`, and `ListStandingWarnings` and
`WatchStandingWarnings` (clause 3.17). `WatchNodes` and `WatchJoinTokens` are
authorized as `ListNodes` and `ListJoinTokens` are. The `iam` service at
`v1alpha1` holds `IssueLoginCode` and `ExchangeLoginCode` (ARFC 0002 section
3.7), `GetCaller` (clause 3.16), and `CreateSessionToken`,
`RefreshSessionToken`, `RevokeSessionToken`, and `RevokeUserSessions` (ARFC
0007 section 3.12). The
CLI of ARFC 0019 clauses 3.1.11, 3.1.12, 3.1.14, and 3.1.16 calls the methods
those clauses name, and the console (ARFC 0019 section 3.2) calls any of them
but `ExchangeLoginCode`.

3.14 Connect form.

3.14.1 The Connect form is the Connect protocol
(https://connectrpc.com/docs/protocol/), version 1, over HTTP/2 on the API
port, at the paths of clause 3.12. Each unary method is a Connect unary call
and each `Watch` method a Connect server streaming call. It has two encodings:
`proto`, the Protocol Buffers binary encoding, and `json`, the Protocol Buffers
JSON mapping (https://protobuf.dev/programming-guides/json/), which the server
writes with lowerCamelCase field names and reads with those names or the names
in the definitions. Nothing is compressed: a request whose `content-encoding`
or `connect-content-encoding` is other than `identity`, or that holds an
envelope with the compressed flag (value 1) set, fails with Connect code
`unimplemented`, and no response is compressed.

3.14.2 Unary. A call is `POST` with `content-type` `application/proto` or
`application/json`, the header `connect-protocol-version: 1`, and the request
message as its body. Success is HTTP status 200 with the same `content-type`
and the response message as body. A `POST` without that header fails with
Connect code `invalid_argument` and HTTP status 400, as the protocol allows. A
method whose name starts with `Get` or `List` changes nothing, is marked
`idempotency_level = NO_SIDE_EFFECTS` in its definition, and can also be called
as `GET <path>?connect=v1&encoding=<proto|json>&message=<message>`, where
`<message>` is the percent-encoded request, or, with `&base64=1` added, the
unpadded base64url of it. A `GET` whose request target is longer than 8 KiB
fails with `InvalidArgument`.

3.14.3 Streaming. A `Watch` call is `POST` with `content-type`
`application/connect+proto` or `application/connect+json`, the header
`connect-protocol-version: 1`, and a body of exactly one envelope holding the
request. An envelope is 1 byte of flags, the message's length in 4 bytes big
endian, and the message. The response is HTTP status 200 with the same
`content-type` and a sequence of envelopes with flags 0, each holding one
response message, then one end-of-stream envelope, flags 2, whose message is a
JSON object: `{}` when the stream ended without error, else `{"error":
<error>}` with `<error>` as clause 3.14.4 writes it. A failure found before the
first message travels the same way, in the end-of-stream envelope.

3.14.4 Errors. A unary call that fails gets the HTTP status of its code's row
in clause 3.6, `content-type` `application/json`, and the body `{"code":
<code>, "message": <message>, "details": [{"type":
"alloy.common.v1alpha1.Error", "value": <value>, "debug": <debug>}]}`.
`<code>` is the row's gRPC status in lower case snake form (`invalid_argument`,
`aborted`), `<message>` the `Error`'s message, `<value>` the unpadded base64 of
the binary encoded `Error`, and `<debug>` the same `Error` in the JSON mapping.
A path under `/alloy.` that is no method fails with code `unimplemented`, HTTP
status 501, and no `details`.

3.14.5 SigV4 on the Connect form is clause 3.7.1 with these differences. The
HTTP method is the request's, `POST` or `GET`, and the canonical URI is its
path. For `GET` the canonical query string is the request's query parameters,
encoded and sorted as Signature Version 4 says, and for `POST` it is empty. The
signed headers are exactly `content-type` when the request has one, `host`,
`x-amz-content-sha256`, and `x-amz-date`. `x-amz-content-sha256` is the lower
case hexadecimal SHA-256 of the HTTP request body, the envelope included for a
`Watch`, and of the empty string for `GET`.

3.14.6 Session cookie. `CreateSessionToken` and `RefreshSessionToken` (ARFC
0007 section 3.12) take `cookie`. With `cookie` set, on the Connect form, the
response carries the header `set-cookie: __Host-alloy-session=<token>; Path=/;
Secure; HttpOnly; SameSite=Strict; Max-Age=<whole seconds until the token
expires>` and its `token` field is empty. With `cookie` set, on gRPC, the call
fails with `InvalidArgument`. The server takes the cookie
`__Host-alloy-session` as a request's session token only when all of these
hold: the request is on the Connect form, is `POST`, carries
`connect-protocol-version: 1`, carries no `authorization` header, and carries
either no `Origin` header or one whose scheme is `https` and whose host and
port equal those of `:authority`, port 443 where either omits it. A request
that carries the cookie and fails one of these is served as if it carried no
cookie. A request whose cookie the server takes and that fails authentication
(ARFC 0007 clause 3.9.12), and every successful `RevokeSessionToken`
authenticated by the cookie, gets the header `set-cookie:
__Host-alloy-session=; Path=/; Secure; HttpOnly; SameSite=Strict; Max-Age=0`.
The `__Host-` prefix and the attributes mean what RFC 6265bis says
(https://datatracker.ietf.org/doc/draft-ietf-httpbis-rfc6265bis/).

3.14.7 No response on the API port carries a header whose name starts with
`access-control-`, and an `OPTIONS` request gets HTTP status 405 (clause
3.12.1).

3.15 Watch.

3.15.1 Each kind's `Watch<Kinds>` method takes `revision`, an unsigned 64-bit
integer, and the fields of its `List` that select resources: `parent` where the
kind has one, and `filter`. It streams `Watch<Kinds>Response` messages, each
with `revision`, `complete`, and `events`. Each event has a `type` of the enum
`alloy.common.v1alpha1.WatchEventType`, which is `EXISTING`, `ADDED`,
`MODIFIED`, or `DELETED`, and the resource, in the field named after the kind
in snake case. A `DELETED` event carries the resource as it last was. A
revision is a commit revision of the consensus group that holds the kind (ARFC
0006 clause 3.7). Revisions of different kinds are not compared.

3.15.2 With `revision` 0, the stream starts with the state at one revision R:
every matching resource once, in `EXISTING` events, in ascending order of name,
in as many messages as the size limit needs, every one carrying R, the last
with `complete` set and the others without. When nothing matches, that is one
message with no events, R, and `complete` set. It then continues from R as
3.15.4 says.

3.15.3 With `revision` R above 0, the stream starts with one message with no
events, R, and `complete` set, then continues from R as 3.15.4 says. A revision
that is compacted fails the call with `Compacted` before any message, and the
server must not compact a revision committed less than 5 minutes before. Any
control node accepts a revision that another delivered. A revision above the
latest the answering node has applied, but not above the group's commit
revision, which the node learns from the group's leader as for a linearizable
read, makes the node wait until it has applied that revision, at most 5
seconds, and then start the stream as above. If it has not applied it by then,
or cannot learn the group's commit revision, the call fails with `Unavailable`
and a retry delay of 1 second. A revision above the group's commit revision
fails with `InvalidArgument`.

3.15.4 After R, the stream delivers every committed change to a matching
resource once, in commit order. The changes of one commit come with that
commit's revision, in consecutive messages, the last of them with `complete`
set and the others without. A resource that comes to match is `ADDED`, one that
changes and still matches is `MODIFIED`, and one that is deleted or stops
matching is `DELETED`. A client resumes a broken stream by calling again with
the revision of the last message it received with `complete` set, on the same
control node or any other, and so misses no change and receives none twice.

3.15.5 Keepalive. When 10 seconds pass with no message on a stream, the server
sends one with no events, `complete` set, and the latest revision through which
the stream is complete, which may be above that of the last change it carried.
A client must treat a stream on which no message arrived for 25 seconds as
broken.

3.15.6 Ending. A Watch is authorized as its kind's `List` when it starts, and
carries only resources that `List` would return to the same caller. The server
ends a stream with `Unauthenticated` within 5 seconds of the revocation of the
credential that authenticated it or of its session (ARFC 0007 clause 3.12.3),
or of the disabling of its principal (ARFC 0007 section 2.2), and at the expiry
of the session token or temporary key that authenticated it. It ends a stream
with `PermissionDenied` within 5 seconds of a policy change after which that
`List` would be refused. A stream otherwise lasts until the client closes it.

3.15.7 Bounds. A control node holds at most 64 open Watch streams per
principal, and one more fails with `RateLimited` and a retry delay of 5
seconds. It holds at most 10,000 in all (ARFC 0006 section 7), and one more
fails with `Unavailable`. A stream holds at most 1,000 messages, or 16 MiB,
that the client has not yet read. A change that would pass either bound ends
the stream with `Unavailable` and a retry delay of 1 second, and the client
resumes as 3.15.4 says.

3.16 `GetCaller` in `iam` returns the caller as its credential establishes it.
It evaluates no policy, since it tells a caller only what its own credential
already establishes. The response has `principal`, the principal's full name,
or `root@<node name>` for a local admin (ARFC 0007 clause 3.10.1);
`principal_kind`, one of `USER`, `SERVICE_ACCOUNT`, `INSTANCE_ROLE`, and
`LOCAL_ADMIN`; `org`, the full name of the principal's org, empty for a local
admin; `cluster_roles`, the names of the cluster roles it holds (`cloud-admin`,
`cloud-viewer`); `default_project`, the full name of the org's project named
`default` when there is one, else empty; `credential`, one of `ACCESS_KEY`,
`SESSION_TOKEN`, `TEMPORARY_KEY`, and `LOCAL_ADMIN`; `access_key_id` when an
access key signed the call; and, for a session token, `expire_time`, the
token's expiry, and `session_expire_time`, the latest expiry a token of its
session can have (ARFC 0007 clause 3.12.2). It returns no secret and no token.

3.17 Standing warnings. The `cluster` service holds the kind `StandingWarning`:
the standing warnings of ARFC 0019 clause 3.2.5 and ARFC 0007 clause 3.10.8.
Its fields are `name`, `standing-warnings/<id>` with a generated `<id>` that
stays the same while the warning stands; `kind`, of the enum
`StandingWarningKind`; `resource`, the full name of the affected resource,
empty when that is the cluster; `message` (rule A11); `next_step`, the
`alloy.common.v1alpha1.NextStep` of the command that removes the cause, empty
when none does; and `create_time`. The kinds, as the CLI and console show them
(rule A5), are `single-anchor`, `unprotected-pool`, `relay-only-node`,
`expiring-record`, `advertise-address-gone`, `mixed-releases`, and
`local-admin-used`. A later kind joins the enum with the clause that raises it.
A warning is listed within 60 seconds of its cause arising, and no longer
listed 60 seconds after its cause ends. `ListStandingWarnings` lists them as
clause 3.4 says and `WatchStandingWarnings` watches them as clause 3.15 says,
both authorized as `cluster:ListStandingWarnings`, which the cluster roles
`cloud-admin` and `cloud-viewer` allow and no other built-in role does.

3.18 OpenAPI. `GET /openapi.json` returns, with no authentication and with
`content-type` `application/json`, an OpenAPI 3.1.0 document
(https://spec.openapis.org/oas/v3.1.0) of the Connect form of the answering
node's release. It has one `POST` operation per unary method, at the method's
path, with `application/json` request and response bodies whose schemas follow
the JSON mapping; the required `connect-protocol-version` header; the
documentation of the method and its fields (clause 3.11) and an example request
(rule A19); the two security schemes, SigV4 in `authorization` and `Bearer`;
and the error body of clause 3.14.4 for each error response. Each `Watch`
method is a `POST` operation with `application/connect+json` bodies and a
description of the envelopes of clause 3.14.3. `cargo xtask` generates the
document from the files under `proto/`, and it is never edited by hand.

## 6. Failure behavior

Without control plane quorum, mutating calls fail with `Unavailable`, and reads
succeed when the caller allows stale reads. A call that times out at the client
may have taken effect: clients retry with the same idempotency key.

## 7. Limits

4 MiB per request, per response, and per message of a stream, larger payloads
go through the object and registry data paths. 8 KiB per `GET` request target
on the Connect form. 64 open Watch streams per principal and 10,000 per control
node, and per stream 1,000 unread messages or 16 MiB (clause 3.15.7). 1,000 items per page. 256 characters per idempotency key. 64
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
neither side keeps, never a key or nonce Alloy keeps (ADR 0042).

Browsers. A page of another site cannot use a user's session. Script cannot
read the session cookie, since it is `HttpOnly`, and `SameSite=Strict` keeps
the browser from sending it with a request that another site starts (clause
3.14.6). A page of another origin on the same site, which `SameSite` does not
separate, cannot make a request that the cookie authenticates either: such a
request is a `POST` with the header `connect-protocol-version` and a
`content-type` of the Connect form, neither of which is CORS-safelisted, so the
browser first sends a preflight `OPTIONS`
(https://fetch.spec.whatwg.org/#cors-preflight-fetch), which the server
refuses with no `access-control-` header (clause 3.14.7). A request that
arrives anyway with the `Origin` of another origin is served without the
cookie. A form or link of another site can send neither the header nor such a
`content-type`, and no response is readable by another origin, since none
carries a CORS header. Upholds I-02, I-15, I-20, I-22.
