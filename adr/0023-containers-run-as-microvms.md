# ADR 0023: Containers run as microVMs

- Status: accepted
- Subject: compute
- Specs: arfc-0011

## Context

Shared-kernel containers are not a tenant boundary.

## Decision

A container resource runs inside its own microVM with an Alloy kernel and `alloy-init`. It has the same network, storage, and identity model as an instance.

## Consequences

- One isolation model for both kinds of guest.
- Start time depends on a cached image and a small kernel.

## Alternatives rejected

- Namespaces as the default isolation: not safe between organizations.
