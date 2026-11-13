# ADR 0036: alloy-sim is written in-house on the alloy-rt seams

- Status: accepted
- Subject: engineering
- Specs: arfc-0005

## Context

ARFC 0005 asked whether to build the simulator on an existing deterministic
runtime. `research/R-0001.md` found that `turmoil` cannot choose the next runnable
task and missed the 3.2.3 time budget by orders of magnitude, that `madsim` needs
a `--cfg` build, git patches, and libc interposition, and that `shuttle` has no
network, disk, or clock model. None models the full fault list of section 3.3.

## Decision

`alloy-sim` is written in this repository. It implements the five `alloy-rt`
seams as `mem`, with its own single-thread executor, seeded scheduler, virtual
clock, network model, and disk model. It depends on no simulation or async
runtime library. Designs may be borrowed from those libraries, code may not.

## Consequences

- Clauses 3.2.2 and 3.3 of ARFC 0005 are met by construction, not by adapters.
- Alloy owns roughly 7,500 to 12,000 lines of simulator before the NAT lab
  (R-0001 estimate), and every bug in them.
- Third-party crates that call the operating system or `tokio` directly cannot run
  under the simulator. They stay behind a seam implementation.
- CI checks determinism by running each scenario twice per seed and comparing the
  trace hash (ARFC 0005 clause 3.2.6).

## Alternatives rejected

- Build on `turmoil`: no control of task order, crash only between ticks, too slow.
- Build on `madsim`: `--cfg` build and git patches break `agent/eng/deps.md`,
  Linux and macOS only, thin disk model.
- Build on `shuttle`: a concurrency tester with no network, disk, or clock.
