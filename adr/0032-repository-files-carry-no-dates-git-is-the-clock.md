# ADR 0032: Repository files carry no dates, git is the clock

- Status: accepted
- Subject: process
- Specs: none

## Context

Dates in files vary by session and add nothing git does not record.

## Decision

Task files, reports, ADRs, and specs contain no dates or timestamps. Order and time come from git history.

## Consequences

- Two agents producing the same file produce the same bytes.
- Retention rules refer to git, not to file contents.

## Alternatives rejected

- Dated log entries: a source of variance and merge noise.
