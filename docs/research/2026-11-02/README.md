# Additional native comparators and maintenance behavior — 2026-11-02

Decision: investigate existing PostgreSQL index implementations before building
another compression backend. Two newly inspected projects are relevant candidates,
but neither has been admitted to our frozen comparison protocol or installed.
For the existing pg_search comparison, qualify 0.26.4 and include maintenance under
reader load. This note extends the [October 26 review](../2026-10-26/README.md).

## Two candidate implementations

Sources were inspected November 2. Release dates and inspected development commits
are recorded separately: features documented at these commits are not claims about
everything available in the corresponding release archives.

**pg_vectorsearch.** The latest returned release is
[v0.1.0, October 13](https://github.com/timescale/pg_vectorsearch/releases/tag/v0.1.0).
The [README at 5f71e47](https://github.com/timescale/pg_vectorsearch/blob/5f71e47deb62057e7f8e3bc5c5f964bc343522b0/README.md)
explicitly labels the project alpha despite the release API's non-prerelease flag.
It describes PRISM hierarchical partition routing, RaBitQ candidate scoring and
heap-vector reranking, with support for existing pgvector columns. It requires
PostgreSQL 18+ and documents posting-list rebalancing. Our inference: this is a
candidate for an isolated index comparison over the same embeddings, avoiding a
custom encoding initially. Qualification must cover cosine recall, inserts,
maintenance, restore and permissions. Alpha upgrade limitations preclude treating
it as a production replacement on this evidence. Vendor speed claims are not our
benchmark results.

**pg_pinecone.** The latest returned release is
[v0.11.0, October 26](https://github.com/pinecone-io/pg_pinecone/releases/tag/v0.11.0).
The [README at 6f3d45c](https://github.com/pinecone-io/pg_pinecone/blob/6f3d45cbd5d24d64b85900e65c82019f1a892275/README.md)
describes pgvector, native and remote engines. Its default native engine applies
filters inside PostgreSQL without an account or network calls; remote uses a
Pinecone service. The documented requirements include PG17/18 and pgvector, and
the source build uses pgrx. Our inference: only the local native engine is a
candidate for the present experiment scope. Verify planner selection, filter
semantics and lifecycle behavior before timing it. Do not enable remote, add
credentials or treat switching engines as an unmeasured scalability guarantee.

Both are retrieval components. Neither README establishes our exact historical
citation contract; comparisons still need the same counted provenance adapter.
Adding either comparator requires a separate reviewed protocol revision, not an
in-place change during optimization. The product license remains undecided.

## Research connection

[RaBitQ, arXiv:2405.12497v1](https://arxiv.org/abs/2405.12497v1), published
May 21, 2024, studies randomized vector quantization with distance-estimation
error bounds. This is older research newly revisited because PRISM uses it.
The paper's abstract supports investigating bounded approximation; it does not
establish filtered cosine recall for our corpus or PostgreSQL memory savings.
Our inference: compare total index/heap/resident memory and reranking costs,
not only compressed code size. Keep exact eligible-set ground truth independent
of the approximate access path. No RaBitQ or TurboQuant implementation was added.

The previously tracked LiPQ and VADER submission histories remain v1 and v2,
respectively. This pass did not reproduce any paper's experiments.

## pg_search 0.26.4: sustained reads can affect reclamation

[0.26.4](https://github.com/paradedb/paradedb/releases/tag/v0.26.4) was published
October 29 at 22:12:59 UTC; its tag resolves to
`727a888fd2801f013d765b1936c6e8eb3a3edf15`.

- [#6879](https://github.com/paradedb/paradedb/pull/6879), merged October 28 and
  [backported in #7072](https://github.com/paradedb/paradedb/pull/7072), changes
  admission around VACUUM cleanup so continuous new readers cannot indefinitely
  replenish the users that cleanup waits for. Upstream reports a real-autovacuum
  regression test. Implication: a future sustained workload should record VACUUM
  completion and reclaimed storage as well as foreground latency.
- [#6740](https://github.com/paradedb/paradedb/pull/6740), merged October 24 and
  [backported in #7016](https://github.com/paradedb/paradedb/pull/7016), makes merges
  and VACUUM's merger wait interruptible. It explicitly identifies cleanup of
  interrupted merge output as separate work. Do not infer complete cancellation
  cleanup from interruptibility alone; verify cancellation recovery and subsequent
  space reclamation in the exact comparator build.

These are upstream findings, not evidence of equivalent bugs in pg-evidence.
This review does not establish the disposition of every follow-up fix in 0.26.4.

## Readiness and verification

PostgreSQL's [version table](https://www.postgresql.org/support/versioning/) still
lists 18.6. Latest queried pgrx, pgvector and pgvectorscale releases/tags remain
0.19.3, 0.8.7 and 0.9.1. [sources.json](sources.json) retains dates and primary
references. This is a bounded review, not an audit of all unreleased commits.

Project main and PR #3 are unchanged. The baseline's known response-budget and
serialization differences and the missing frozen vector ground truth still prevent
the proposed optimization comparisons. No candidate, database benchmark or holdout
ran; no performance failure is concealed by that readiness decision. The evaluator,
fixtures, seeds and thresholds were unchanged, with hashes recorded in
[run-manifest.json](run-manifest.json).

Fresh host verification: 59 benchmark tests passed, 28 packaging tests passed with
two skipped, and protocol validation passed; raw logs accompany this note. No
native build or database suite was rerun for this documentation-only update.
No merge, release, workflow-scope expansion or paid inference occurred.
