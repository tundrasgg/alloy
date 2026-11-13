# Fuzz targets

One libFuzzer target per public decoder of `alloy-wire`, as
`agent/eng/testing.md` requires. Each target checks that decoding never panics
and that an input that decodes encodes back to exactly the same bytes.

| Target | Decoder |
|---|---|
| `message` | `Message::decode`, and through it the initiation, response, cookie reply and transport decoders |
| `inner_frame` | `InnerFrame::decode` and `InnerFrame::decode_reassembled`, fragment header included. The first 3 bytes of an input are the VNI, little endian |
| `initiation_payload` | `InitiationPayload::decode` |
| `response_payload` | `ResponsePayload::decode` |

This directory is a Cargo workspace of its own, left out of the root one, so
`cargo xtask ci` does not build it. The targets need `cargo-fuzz` and a nightly
toolchain:

```
cargo install --locked cargo-fuzz
cargo +nightly fuzz run message fuzz/corpus/message fuzz/seeds/message -- -max_total_time=60
```

Run from the repository root. libFuzzer adds the inputs it finds to the first
directory, `fuzz/corpus/<target>`, which git ignores, and reads the committed
seeds from the second. A crash is written to `fuzz/artifacts/<target>/`.

On Windows with the MSVC toolchain, the AddressSanitizer runtime
`clang_rt.asan_dynamic-x86_64.dll` must be on `PATH`. Visual Studio ships it in
its `VC\Tools\MSVC\<version>\bin\Hostx64\x64` directory.

## Dependencies

`deny.toml` in this directory is the allowlist of the fuzz graph. It is apart
from the root `deny.toml`, so that the NCSA exception for `libfuzzer-sys` and
the fuzz-only crates never apply to the root workspace (ADR 0060). cargo-deny
finds it from the fuzz manifest:

```
cargo deny --manifest-path fuzz/Cargo.toml --locked --workspace check
```

## Seeds

`seeds/<target>/` holds the valid messages of the unit tests in
`crates/alloy-wire/src`, one file per message. `examples/write_seeds.rs` writes
them. After a change to the wire format, rerun it and commit the result:

```
cargo +nightly run --manifest-path fuzz/Cargo.toml --example write_seeds
```
