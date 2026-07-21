# 0016 - Object storage with OpenDAL and an S3-first key model

- Status: accepted
- Date: 2026-07-13
- Decision makers: Oscar González
- Related: ADRs 0005, 0012, 0015, and 0017

## Context

Object storage is flat rather than hierarchical. S3 provides UTF-8 keys,
prefixes, multipart upload, and server-side CopyObject, but no native directory,
rename, or append. The provider needs safe create-new behaviour, honest
capabilities, injected credentials, and realistic tests without making Docker a
pull-request requirement.

## Decision

- Use OpenDAL 0.58 with default features disabled and only `services-s3`
  enabled. It provides feature-gated paths to GCS/Azure, lazy pagination, and
  conditional writes. Keep OpenDAL types behind `norte-vfs-object` except for
  the injected `Operator` constructor used by the connection layer.
- Model directories with `key/` marker objects plus a one-item prefix probe.
  `mkdir` creates the marker after parent and conflict checks. `stat` resolves
  file, marker, prefix-with-children, then not found. Files take precedence in
  the externally possible `x` plus `x/` ambiguity.
- Compose keys only from validated `VPath` segments. Reject non-UTF-8 segments,
  keys above S3's 1,024-byte limit, empty or structural list results, and
  leading/trailing Unicode whitespace. The whitespace restriction prevents
  OpenDAL's current `trim()` normalization from silently changing a valid S3
  key (#48).
- Treat the object writer or multipart upload as invisible staging. Perform an
  up-front absence check and use `if_not_exists(true)` so an honest server
  enforces create-new at commit time. Abort cancels multipart upload. The
  up-front check also provides a safe fallback for imperfect S3-compatible
  servers.
- Defer persistent multipart resume. OpenDAL does not expose upload IDs or
  ListParts, so `open_resumable` safely reports zero and cancellation aborts.
  Future options are an upstream OpenDAL API, small raw S3 calls, or a scoped
  AWS SDK dependency.
- Implement `copy_native` with conditional CopyObject. Check both file and
  directory destinations first and advertise `SERVER_COPY` only when the
  operator supports copy. Long multipart copies currently have no intermediate
  cancellation point (#51).
- Stream lazy listings, ranged reads, and use explicit stats before remove so
  OpenDAL's idempotent delete does not hide `NotFound`. Rename is copy then
  delete; prefix rename is copy-all then delete-all, non-atomic and O(n), with
  failures leaving duplicates rather than data loss.
- Advertise `CASE_SENSITIVE`, `CASE_PRESERVING`, conditional `SERVER_COPY`, and
  `max_path = 1024`. Do not advertise append, random write, symlinks, atomic
  rename, stable node IDs, or trash before ADR 0019.
- Build the operator in the connection layer from a public access-key ID and a
  separately resolved secret. Explicit credentials disable ambient config and
  metadata; `auth=agent` deliberately uses the ambient AWS chain.

## Testing

Use four layers because no single in-process S3 implementation matches the
contract:

1. Run the provider contract over OpenDAL's filesystem service with atomic
   staging for ordinary behaviour.
2. Run S3-specific multipart, ranges, and conditional-write tests against an
   in-process s3s server where its behaviour is faithful.
3. Use a minimal dishonest HTTP server for injected hostile listing keys.
4. Run complete MinIO compatibility, long-key, marker, copy, and conditional
   tests in nightly testcontainers CI.

## Consequences

The same provider can gain GCS/Azure backends through features and supplies the
first real `copy_native` implementation. Conditional writes give race-free
create-new semantics on conforming servers. Multipart resume remains deferred;
until then an interrupted S3 upload restarts safely.

OpenDAL adds a large HTTP/signing dependency tree. Names are necessarily
UTF-8-only under S3. Prefix rename/delete is non-atomic, and bucket-only VPath
authorities can collide in caches when the same bucket name exists at multiple
endpoints. Upstream trimming, large-tree materialization (#49), and test gaps
for long/external keys (#50) remain tracked.
