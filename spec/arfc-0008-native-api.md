# ARFC 0008: Native API

- Status: accepted
- Depends on: 0006, 0007
- Crates: alloy-proto, alloy-api
- Decisions: 0026

## 1. Purpose

One API is the source of truth for everything Alloy does. Every other surface is a
client of it.

## 2. Model

The API is defined in Protocol Buffers under `proto/alloy/<service>/<version>/`.
It is served as gRPC and as JSON over HTTP generated from the same definitions,
with an OpenAPI document published at `/openapi.json`.

Services: `cluster`, `iam`, `compute`, `network`, `block`, `object`, `registry`,
`audit`, `metering`. Each service has resource kinds, and each kind has the
standard methods `Create`, `Get`, `List`, `Update`, `Delete`, `Watch`, plus custom
methods named `Verb` + `Kind`.

## 3. Behavior

3.1 Versions are `v1alpha1`, `v1beta1`, `v1`. Alpha may change freely. Beta
changes only compatibly within a minor release. A stable version never breaks: no
field is removed, renumbered, or changes meaning.

3.2 Every mutating method accepts an `idempotency_key`. Repeating a call with the
same key within 24 hours must return the first result and must not act twice.

3.3 Operations that do not finish within the call return an `Operation` resource
with `done`, `error`, `result`, and progress. Operations are kept for 7 days.

3.4 `List` paginates with an opaque `page_token`, default page size 100, maximum
1,000, and accepts `filter` (a label and field expression) and `order_by`.

3.5 `Update` takes a field mask and the `generation` read. A stale generation
fails with `Conflict`.

3.6 Errors carry a stable `code`, a human `message`, the `resource` concerned, and
`details`. Codes are defined once, in `proto/alloy/common/errors.proto`, and are
never reused. A message says what happened, why, and what to do next.

3.7 Authentication: a session token, or SigV4 with an access key. Every call is
authorized (ARFC 0007) and, when mutating, audited (ARFC 0018).

3.8 The API is served on TLS 1.3 only, on port 443 of every control node, at
`https://api.<cluster-domain>` or by address. The certificate comes from ACME when
the cluster has a public name, otherwise from the API CA whose fingerprint the CLI
pins at first login.

3.9 Rate limits apply per principal, default 100 requests per second with a burst
of 200, and return `RateLimited` with a retry delay.

3.10 `alloy-proto` generates the Rust client. The CLI and console use only
generated clients.

3.11 Every field, method, and error code has documentation in the definition. CI
fails on an undocumented item and on any incompatible change to a beta or stable
version.

## 6. Failure behavior

Without control plane quorum, mutating calls fail with `Unavailable`, and reads
succeed when the caller allows stale reads. A call that times out at the client
may have taken effect: clients retry with the same idempotency key.

## 7. Limits

4 MiB per request and per response, larger payloads go through the object and
registry data paths. 1,000 items per page. 256 characters per idempotency key. 64
labels per resource, keys and values up to 63 and 255 characters.

## 8. Security

Upholds I-02, I-15, I-22.
