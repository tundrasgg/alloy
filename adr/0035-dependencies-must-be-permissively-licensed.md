# ADR 0035: Dependencies must be permissively licensed

- Status: accepted
- Subject: engineering
- Specs: none

## Context

The dual license only holds if everything linked in permits it.

## Decision

Only dependencies under MIT, Apache-2.0, BSD, ISC, Zlib, or similar permissive terms are allowed, enforced by an allowlist in `deny.toml`. No copyleft code is linked or vendored.

## Consequences

- Some well-known components are unavailable and are written in-house or run as separate programs.
- Each new dependency needs a research note.

## Alternatives rejected

- Case-by-case copyleft exceptions: erodes the license promise.
