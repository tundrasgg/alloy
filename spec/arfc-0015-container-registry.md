# ARFC 0015: Container registry

- Status: accepted
- Depends on: 0007, 0014
- Crates: alloy-registry
- Decisions: 0027

## 1. Purpose

An OCI registry inside the cluster, so images live next to the workloads that run
them and follow the same tenancy and access rules.

## 2. Model

A **repository** belongs to a project and is addressed as
`registry.<cluster-domain>/<org>/<project>/<name>`. Blobs are stored once per
project in the `blob-backend` and are content addressed by SHA-256. Manifests,
tags, and referrers are index entries.

## 3. Behavior

3.1 The registry implements the OCI Distribution Specification version 1.1: pull,
push (monolithic and chunked), cross-repository mount, tag listing, catalog scoped
to what the caller may see, deletion, and the referrers API.

3.2 It must pass the OCI distribution conformance suite for the pull, push,
discovery, and management categories.

3.3 Authentication follows the token flow used by container clients: a `401` with
a challenge, a token request with an access key or session token, then a bearer
token scoped to repositories and actions. Actions map to `registry:` IAM actions.
Tokens last 5 minutes.

3.4 A repository is private by default. Public pull requires the repository and
its project to both allow it.

3.5 Tags may be made immutable per repository. A push to an immutable tag fails
with `DENIED`.

3.6 Unreferenced blobs and untagged manifests are collected after a configurable
period, default 7 days. Collection must never remove a blob that an upload in
progress or a concurrent push references.

3.7 A repository may be a pull-through cache of an upstream registry: a pull of a
missing manifest or blob fetches it upstream, stores it, and serves it.

3.8 Container workloads on Alloy (ARFC 0011) pull through the node, which
authenticates with the workload's project scope and caches by digest.

3.9 Uploads and downloads stream. The registry must not hold a whole blob in
memory.

## 6. Failure behavior

An interrupted upload can be resumed by its upload id for 24 hours. A blob whose
digest does not match what was pushed is rejected and discarded.

## 7. Limits

10 GiB per blob by default, configurable per project. 4 MiB per manifest. 255
characters per repository path. 1,000 tags per list page.

## 8. Security

Upholds I-02, I-05. Digests are verified on push and on pull-through.

## 9. Open questions

- Signature verification policy for workloads. Product brief to be cut in M8.
