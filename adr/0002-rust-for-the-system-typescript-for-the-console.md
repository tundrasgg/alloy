# ADR 0002: Rust for the system, TypeScript for the console

- Status: accepted
- Subject: project
- Specs: arfc-0019

## Context

One systems language keeps the simulator, the dataplane, and the control plane in one toolchain.

## Decision

All backend code is Rust in one Cargo workspace. The console is TypeScript. No other languages carry product behavior.

## Consequences

- Sans-I/O Rust code runs unchanged in production, simulation, and fuzzing.
- Guest-side code (`alloy-init`) is Rust as well.

## Alternatives rejected

- A second backend language for control plane code: two runtimes, two simulators.
