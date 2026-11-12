# ADR 0026: The native API is the source of truth, EC2 and S3 are compatibility surfaces

- Status: accepted
- Subject: api
- Specs: arfc-0008, arfc-0014, arfc-0016

## Context

Compatibility APIs carry another system's model and limits.

## Decision

One native API defines every resource. The EC2 surface is a stateless translator over it. The S3 surface uses the same identity and policy engine. Compatibility surfaces match the original's behavior and never extend it.

## Consequences

- Existing tools work by changing endpoint and credentials.
- New capabilities appear in the native API first.

## Alternatives rejected

- An EC2-shaped native API: inherits constraints Alloy does not have.
