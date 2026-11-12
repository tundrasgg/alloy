# ADR 0025: Alloy has its own storage system

- Status: accepted
- Subject: storage
- Specs: arfc-0012, arfc-0013, arfc-0014

## Context

Storage has to start on one disk with one command, scale to racks, follow Alloy's tenancy, and stay under Alloy's license.

## Decision

Alloy implements its own disks, placement, replicated chunk store, block volumes, object index, and S3 front end. Every layer is a seam, with native and in-memory implementations from the start.

## Consequences

- No external storage system to deploy or license.
- Node-local and replicated volumes share one code path.
- This is the largest body of work in the project and is tested mainly by simulation.

## Alternatives rejected

- Embedding an existing object store or block system: license, operations, and tenancy mismatches.
