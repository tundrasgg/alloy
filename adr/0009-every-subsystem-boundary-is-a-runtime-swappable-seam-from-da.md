# ADR 0009: Every subsystem boundary is a runtime-swappable seam from day zero

- Status: accepted
- Subject: engineering
- Specs: arfc-0001

## Context

Hardware, scale, and preference vary per cluster. Retrofitting seams later costs rewrites.

## Decision

Subsystem boundaries are object-safe traits in a runtime registry, chosen by configuration and control plane state. Cargo features never select behavior.

## Consequences

- Backends can be mixed and migrated per pool or node.
- Each seam needs an in-memory implementation and a conformance suite.
- Hot path traits are batch oriented.

## Alternatives rejected

- Compile-time selection by features: one backend per binary and untested combinations.
