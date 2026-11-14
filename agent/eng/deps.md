# Engineering: dependencies

## The allowlist

`deny.toml` at the repository root is the allowlist. `cargo deny check` runs in
`cargo xtask ci`. A crate that is not on the list does not build in CI.

## Adding a dependency

Adding a crate is a class N decision. The task that adds it adds the crate to
the `allow` list in `deny.toml` (`agent/protocol/tasks.md`, Shared workspace
files) and records in its report, each with its source (a URL, or a command and
its output such as `cargo tree`):

- the license,
- maintenance: the date of the last release or meaningful commit,
- the tree: how many crates it adds to `Cargo.lock`, and that each passes the
  rules below.

The lead records the decision in `plan/decisions.md`. No research note is needed,
unless the choice is between alternatives (two candidate crates, or a crate
against writing it ourselves): then a research task compares them first, with
the fields in `agent/roles/researcher.md`.

A crate qualifies when all hold:

- **License** is compatible with MIT OR Apache-2.0: MIT, Apache-2.0, BSD-2-Clause,
  BSD-3-Clause, ISC, Zlib, Unicode-3.0, or the same dual license. No copyleft of
  any strength: no GPL, AGPL, LGPL, MPL, or SSPL code is linked or vendored.
  Two exceptions, each confined by its ADR. `libfuzzer-sys` may carry NCSA, in
  fuzz targets under `fuzz/` only, and no other crate may use NCSA (ADR 0060).
  The Windows release archives may ship the signed Wintun DLL unmodified under
  its own license (ADR 0062). The DLL is not a crate, and a crate that binds
  the Wintun API passes these rules like any other.
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

## npm packages (the console)

The console under `ui/console/` uses npm (ADR 0070). `cargo deny` does not see
npm, so `cargo xtask console` checks these rules over every package in
`ui/console/package-lock.json` and fails, naming the package, on any breach.

- **Pinned**: every entry of `dependencies` and `devDependencies` in
  `ui/console/package.json` is an exact version, with no range, tag, URL, git
  revision, or path. The lockfile is committed and `npm ci` installs from it.
- **No install scripts**: packages install with `--ignore-scripts`.
- **Runtime packages**, the ones that `dependencies` brings into the built
  console, are the four that ADR 0070 names, with their own dependencies. They
  pass every rule of a crate above: license, maintained, necessary, contained.
- **Dev packages**, everything else in the lockfile, never ship in
  `ui/console/dist/`. Each is maintained and contained as a crate is, and its
  license is one that a crate may carry, or one of `0BSD`, `BlueOak-1.0.0`,
  `CC0-1.0`, and `CC-BY-4.0`. A license is read from the package's `license`
  field as an SPDX expression; an `OR` passes when one side passes, and an
  `AND` when both do. A missing or unparsable field fails.
- Adding a dev package is class R when it passes; adding a runtime package
  needs an ADR (ADR 0070). Adding a license to either list is a `process` task.

## Not dependencies

- Do not vendor source into the tree without an ADR.
- Do not depend on a git revision or a path outside the workspace.
- Do not shell out to a system binary for product behavior unless an ARFC names
  that binary as part of a seam implementation.
- External programs Alloy runs as separate processes (a hypervisor, for example)
  are seam implementations with their own license review recorded in an ADR.
