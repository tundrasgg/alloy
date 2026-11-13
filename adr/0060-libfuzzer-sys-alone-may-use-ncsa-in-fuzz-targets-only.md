# ADR 0060: libfuzzer-sys alone may use NCSA, in fuzz targets only

- Status: accepted
- Subject: engineering
- Specs: none

## Context

The standing fuzzing choice is `cargo-fuzz` (libFuzzer). Its crate `libfuzzer-sys`
declares `(MIT OR Apache-2.0) AND NCSA` for the libFuzzer sources it vendors, and
NCSA is not on the license list in `agent/eng/deps.md`, so `cargo deny` rejects
it. NCSA is permissive, but listing it would admit it for every crate.

## Decision

- `libfuzzer-sys` may be used under its declared license
  `(MIT OR Apache-2.0) AND NCSA`. It is the only crate allowed NCSA.
- It is a dependency only of fuzz targets, in the fuzz package under `fuzz/`,
  which is not a member of the root workspace. No crate under `crates/`, `ui/`
  or `xtask/` depends on it, directly or transitively.
- `deny.toml` admits NCSA through one license exception naming `libfuzzer-sys`,
  never through its global license list.
- Fuzz binaries are never shipped: no release artifact contains one.

## Consequences

- Fuzzing keeps libFuzzer, and the fuzz graph passes `cargo deny`.
- Any other crate that declares NCSA is still rejected and needs its own ADR.
- Shipping or linking libFuzzer into a product binary needs a new ADR.

## Alternatives rejected

- Add NCSA to the global license list: any product crate could then use it.
- Replace libFuzzer with a fuzzer that vendors no libFuzzer: reopens a settled
  choice for code that never ships.
- Restate the crate's license with a `licenses.clarify` entry: overrides the
  crate's own declaration and still needs a licensing decision.
