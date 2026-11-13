# Engineering: dependencies

## The allowlist

`deny.toml` at the repository root is the allowlist. `cargo deny check` runs in
`cargo xtask ci`. A crate that is not on the list does not build in CI.

## Adding a dependency

Adding a crate is a class N decision that needs a research note covering the
fields in `agent/roles/researcher.md`. The lead adds it to `deny.toml` in the
task that needs it and records it in `plan/decisions.md`.

A crate qualifies when all hold:

- **License** is compatible with MIT OR Apache-2.0: MIT, Apache-2.0, BSD-2-Clause,
  BSD-3-Clause, ISC, Zlib, Unicode-3.0, or the same dual license. No copyleft of
  any strength: no GPL, AGPL, LGPL, MPL, or SSPL code is linked or vendored.
  One exception: `libfuzzer-sys` may carry NCSA, in fuzz targets under `fuzz/`
  only, and no other crate may use NCSA (ADR 0060).
- **Maintained**: a release or meaningful commit in the last twelve months, or the
  crate is small and finished.
- **Necessary**: writing it ourselves would take more than a task or would be
  cryptography.
- **Contained**: its transitive tree adds no crate that fails these rules.

## Standing choices

These are settled, see the ADRs. Do not reopen them in a build task.

| Need | Choice |
|---|---|
| Async runtime (production `alloy-rt`) | `tokio` |
| Serialization of APIs | Protocol Buffers via `prost` |
| Errors | `thiserror` |
| Logging and tracing | `tracing` |
| Property tests | `proptest` |
| Fuzzing | `cargo-fuzz` (libFuzzer) |
| Cryptographic primitives | see `agent/eng/crypto.md` |

Choices marked "default, pending research" in an ARFC are confirmed or replaced by
the research task that the ARFC names.

## Not dependencies

- Do not vendor source into the tree without an ADR.
- Do not depend on a git revision or a path outside the workspace.
- Do not shell out to a system binary for product behavior unless an ARFC names
  that binary as part of a seam implementation.
- External programs Alloy runs as separate processes (a hypervisor, for example)
  are seam implementations with their own license review recorded in an ADR.
