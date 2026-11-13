# ADR 0042: Nodes talk to the control plane over AL, clients to the API over gRPC

- Status: accepted
- Subject: api
- Specs: arfc-0002, arfc-0004, arfc-0008

## Context

M11 rung 2 runs `init`, `join`, and `alloy node list` on two Linux nodes and a Windows client before the mesh dataplane exists. ARFC 0002 gave no bytes for the join messages and no default mesh port, ARFC 0008 no API wire, and both the API and the `stream` underlay wanted TCP 443. Every `rustls` provider draws handshake randomness from its own generator (R-0008, ADR 0053), against `agent/eng/crypto.md` rule 4. R-0007 compared API transports. The T-0088 review found a moved data directory recorded nowhere, and nothing guarding the directories above the Windows one.

## Decision

- Node channel: every message between a node's `alloyd` and the control plane (join, approval polling, renewal, withdrawal, leave, status) is a `alloy.node.v1alpha1.NodeMessage` in one UDP datagram, ports 7477, in an IPv6 packet between infrastructure addresses on VNI 0 of the AL session the node holds with an anchor, at most 16 KiB. The source address must be the one of the mesh key the session authenticated. One request is outstanding per session, resent every second, and the answering side keeps the reply to the highest `request_id` so a request is handled once. An anchor without `control` forwards to a control node. The anchor replies only at the node's current endpoint, so a joiner runs one reflection exchange first (ARFC 0002 section 3.13, ARFC 0004 section 3.1.2).
- Until the mesh runs, the node channel is the only traffic between nodes, directly over the underlay to an anchor's published address. `al0`, relays, signaling, and paths between nodes wait for M11 rung 3; then the same packets ride `al0`.
- The mesh port is UDP 7476, unassigned at IANA and below the Linux and Windows ephemeral ranges.
- The API is gRPC over HTTP/2 on TLS 1.3, built on `tonic` over `prost`. No client streaming. JSON over HTTP, OpenAPI, and a browser form come with the console. Errors map to one gRPC status, HTTP status, and exit code each, with an `alloy-error-bin` trailer.
- SigV4 signs a gRPC call over `POST`, the method path, `content-type`, `host` (`:authority`), `x-amz-date`, and the SHA-256 of the whole body, scope `<date>/global/alloy/aws4_request`, 5 minutes of clock skew. The local admin socket speaks the same gRPC without TLS.
- One TCP port per node, `api_port`, default 443, serves the API to ALPN `h2` on control nodes and `stream` to `alloy-al/1` on anchors. Anything else gets `no_application_protocol`. `stream` clients accept any certificate, since AL authenticates the anchor.
- TLS randomness: under the `os` runtime the `aws-lc-rs` provider draws TLS hello randoms and key shares from its own generator, the one exception to rule 4. Every key, nonce, and secret Alloy keeps or sends still comes from `alloy-rt`, and resumption is off. Under any other `net` implementation no TLS runs, and the same bytes go in the clear, the API told from `stream` by the HTTP/2 preface.
- `init` and `join` make no CLI context. Every machine logs in with a login code; root on a control node also has the local admin socket.
- The data directory path is fixed (`/var/lib/alloyd`, `%ProgramData%\Alloy`); `--data-dir` is removed and a mount at the path moves it. On Windows no one but `SYSTEM`, `Administrators`, and `TrustedInstaller` may own, delete, or rename it or a directory above it, and no reparse point is on its path.
- Each node seals its secrets in `secrets` under a 32-byte at-rest key, XChaCha20-Poly1305 with a random 24-byte nonce per write. API certificates have a fixed profile: CA 10 years, server 30 days, renewed at 10 days left.

## Consequences

- Rung 2 needs the AL handshake, transport, and reflection probes, but no TUN, relay, or path manager. The node channel bytes do not change at rung 3.
- `tonic`, `prost`, and their trees enter `deny.toml` by the tasks that build `alloy-proto` and `alloy-api`. The console must reach gRPC through a browser form later (R-0007).
- TLS code is not exercised by the simulator; its tests run under `os` on loopback.
- ARFC 0017's wording on a data directory "as the installed `alloyd` reports it" now always means the default path.
- The cluster key and API CA key live in the secrets of the one control node until multi-voter membership specifies their copy (ARFC 0002 section 9).

## Alternatives rejected

- connect-rust (R-0007 option A): pre-1.0, and `buffa` instead of the standing `prost`.
- Join over the API's TLS: the token pins anchors' mesh keys, not the API CA, and joins would need a second path once the mesh runs.
- New AL control frame types for node messages: changes ARFC 0003, while IPv6 on VNI 0 is what `al0` carries anyway.
- A `rustls` provider fed from `alloy-rt`: `aws-lc-rs` makes key shares inside its C code, so it needs new key exchange glue, which rules 1 and 2 discourage.
- A context made at join: a join token is node trust, not a user credential, and every node would hold a user's key.
- Separate ports for the API and `stream`: `stream` exists to pass networks that allow only TCP 443.
- Record a moved data directory: needs a new reserved object that every release's uninstall reads, where a mount gives the same.
