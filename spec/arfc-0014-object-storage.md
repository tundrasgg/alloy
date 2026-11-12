# ARFC 0014: Object storage

- Status: accepted
- Depends on: 0007, 0012
- Crates: alloy-s3, alloy-meta
- Decisions: 0025, 0026

## 1. Purpose

S3-compatible object storage built on Alloy's own chunk store and its own index,
integrated with Alloy's IAM and tenancy.

## 2. Model

```
 alloy-s3      S3 protocol, authentication, policy, multipart, lifecycle
 index         bucket and object metadata, ordered by key, sharded
 chunk store   object data (ARFC 0012)
```

A **bucket** belongs to a project, lives in one region and one pool, and has a
name unique across the cluster. Its **index** maps `(key, version)` to object
metadata and a manifest of chunks. The index is split into **shards** by key
range. Each shard is a small replicated state machine on storage nodes.

Objects up to 64 KiB are stored inline in the index. Larger objects are chunked
into the pool, and the manifest lists the chunks.

## 3. Behavior

3.1 The service speaks the Amazon S3 REST API with Signature Version 4, including
presigned URLs and chunked upload signing, in both virtual-hosted and path style,
at `s3.<region>.<cluster-domain>` and at a path on the API endpoint.

3.2 Supported in version 1: bucket create, delete, list, and location. Object put,
get, head, delete, batch delete, copy, and range reads. List objects (both
versions of the call), with prefix, delimiter, and continuation. Multipart upload,
all calls. Versioning. Object tagging. Bucket policy. Canned ACLs `private` and
`public-read`. CORS. Lifecycle rules for expiration, noncurrent version
expiration, and aborting incomplete multipart uploads. Server-side encryption with
service keys and with customer-provided keys. Conditional requests. Checksums.

3.3 Not in version 1, returning `NotImplemented`: object lock, replication,
inventory, analytics, event notifications, select, website hosting.

3.4 Consistency: after a successful write, every subsequent read, list, or head
must observe it. Listing is in key order.

3.5 Authorization combines IAM policy and bucket policy by the rules of ARFC 0007.
The S3 action names map one to one to `object:` actions. Anonymous access is
possible only where a bucket policy or ACL allows it and the project permits
public buckets, which is off by default.

3.6 An object's data is written to the chunk store before its index entry commits.
A crash between the two leaves orphan chunks, which a collector removes after 24
hours. A committed index entry must never reference a missing chunk (I-12).

3.7 Shards split at 10 GiB or 20 million entries and merge below a quarter of
that. Splits and merges do not block reads or writes for more than 1 second.

3.8 ETags match S3: the MD5 of the content for single-part objects, and the
multipart form otherwise.

3.9 Compatibility is measured by a pinned subset of the `ceph/s3-tests` suite and
by the AWS CLI and SDK smoke scenarios in `tests/system/s3`. The pinned list is
normative, and widening it is a spec change.

3.10 Usage (bytes and object counts per bucket) is metered per ARFC 0007 and
counted against quota at write time.

## 5. Seams

| Kind | Trait | Implementations | Suite |
|---|---|---|---|
| `blob-backend` | `BlobBackend` | `native`, `mem` | `tests/conformance/blob-backend` |
| `index-store` | `IndexStore` | `native`, `mem` | `tests/conformance/index-store` |

## 6. Failure behavior

Shard without quorum: writes and consistent reads to its key range fail with
`503 SlowDown`. Other ranges are unaffected. Data PG below `min_size`: writes to
it fail, reads continue from surviving members.

## 7. Limits

5 TiB per object. 10,000 parts per upload, 5 MiB to 5 GiB each. 1,024 bytes per
key. 2 KiB of user metadata. 1,000 buckets per project by default. Bucket names
follow S3 rules, 3 to 63 characters.

## 8. Security

Upholds I-02, I-05, I-12, I-15. Customer-provided keys are never stored.

## 9. Open questions

- Choice of the pinned compatibility test list. Research task to be cut in M7.
