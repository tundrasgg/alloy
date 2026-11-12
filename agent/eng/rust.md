# Engineering: Rust

## Toolchain

- The toolchain is pinned in `rust-toolchain.toml`. Do not use features of a newer
  compiler. Raising the pin is a `process` task.
- Edition 2024. `cargo fmt` with the repository's `rustfmt.toml`. `cargo clippy`
  with `-D warnings` and the lint set in the workspace `Cargo.toml`.
- Supported targets: `x86_64-unknown-linux-gnu`, `aarch64-unknown-linux-gnu`. The
  CLI also builds for macOS and Windows.

## Architecture rules

1. **Sans-I/O protocols.** Protocol and consensus logic is a state machine:
   `fn handle(&mut self, input, now) -> outputs`. It owns no sockets, timers,
   tasks, or files. The same code runs in production, in the simulator, and under
   the fuzzer.
2. **All I/O behind `alloy-rt`.** Clock, network, disk, randomness, and task
   spawning are traits. Only `alloy-rt` implementations call the operating system.
   Never call `std::time::Instant::now`, `SystemTime::now`, `tokio::spawn`,
   `std::net`, `std::fs`, or `rand::thread_rng` anywhere else.
3. **Seams are runtime.** Subsystem boundaries are object-safe traits registered
   by name and chosen from configuration at runtime
   (`spec/arfc-0001-runtime-seams.md`). Cargo features select platform support
   only, never behavior or backends.
4. **Batch across seams.** A trait method on a hot path takes and returns batches,
   so that dynamic dispatch costs once per batch.
5. **Determinism.** No `HashMap` iteration order in anything observable: use
   `BTreeMap` or sort. No address-dependent behavior. No unseeded randomness.
6. **Bounded everything.** Every queue, cache, buffer, and retry loop has a stated
   limit and a defined behavior at the limit.

## Errors

- Library crates define error enums with `thiserror`. No `anyhow` and no
  `Box<dyn Error>` in library APIs. Binaries may use `anyhow` at the top level.
- Errors that cross the API carry a stable code from
  `spec/arfc-0008-native-api.md`.
- Panics are bugs. Input from the network, the disk, a guest, or a user never
  panics the process. Parsers are total.
- `unwrap` and `expect` follow `agent/core/4-evidence.md`.

## Unsafe

`unsafe` is allowed only in these crates, and each block carries a `// SAFETY:`
comment stating the invariant: `alloy-rt` (operating system bindings),
`alloy-net` (packet I/O and shared memory rings), `alloy-store` (direct block
I/O). These crates run under Miri where it applies. Every other crate has
`#![forbid(unsafe_code)]`.

## Style

- Public items have doc comments that state behavior, errors, and limits.
- Comments explain why. They do not narrate the code and they do not record
  history.
- Newtypes for every identifier and unit: `NodeId`, `Vni`, `Epoch`, `Bytes`. No
  bare `u64` or `String` ids in signatures.
- Parse at the boundary, then pass typed values. Wire and disk formats are decoded
  in one place per format.
- No global mutable state. No `lazy_static` singletons for behavior.
- Logging uses `tracing` with structured fields. Never log secrets, key material,
  tokens, or tenant payloads.

## Performance

- The dataplane does not allocate per packet and does not take a lock shared
  between queues.
- Performance claims come with a benchmark in the crate and numbers in the report.
- Optimize after a benchmark shows the need, and keep the benchmark.
