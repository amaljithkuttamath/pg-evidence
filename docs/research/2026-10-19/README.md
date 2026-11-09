# Cached filters, adaptive search and dataset readiness — 2026-10-19

Decision: update the proposed pg_search comparator from 0.26.1 to **0.26.2**.
Check cached prepared queries after tenant-setting changes, and record the actual
vector execution path and worker count. These are proposed comparison requirements;
no dependency, evaluator or benchmark threshold changed in this session.

This follows the [October 12 review](../2026-10-12/README.md). Sources were accessed
October 19; publication dates below distinguish new releases from older research.

## New release evidence

[ParadeDB 0.26.2](https://github.com/paradedb/paradedb/releases/tag/v0.26.2) was
published **2026-10-16 02:34:15 UTC**. Its annotated tag resolves to commit
`d22f0fba4da0cc4491f7197b37cd5f3a0da09b12`. Three changes matter here:

| Primary source | Merged | Evidence and specific implication |
|---|---|---|
| [Original fix #6798](https://github.com/paradedb/paradedb/pull/6798), [release backport #6807](https://github.com/paradedb/paradedb/pull/6807) | October 14 | Cached search plans could retain a planning-time value of a STABLE expression, including tenant settings used in indexed RLS predicates. The fix evaluates these expressions during execution. Run repeated prepared searches, then change the tenant setting in the same session and verify allowed IDs and counts against plain SQL. Exercise forced generic plans and automatic plan selection. |
| [Original change #6769](https://github.com/paradedb/paradedb/pull/6769), [backport #6825](https://github.com/paradedb/paradedb/pull/6825) | October 14 / 15 | Enables exact-plan and automatic located-path choices; vector statistics expose per-segment execution details. A filtered query need not follow one fixed approximate path. Capture EXPLAIN ANALYZE VERBOSE with vector statistics in a separate diagnostic pass; identify exact, located and routed work when interpreting recall and latency. |
| [Original fix #6835](https://github.com/paradedb/paradedb/pull/6835), [backport #6842](https://github.com/paradedb/paradedb/pull/6842) | October 15 | Restores vector worker selection for runtime filters after the STABLE-expression change, while retaining execution-time evaluation. Record both planned and launched workers. A timing difference with different parallelism is not evidence of a compression improvement. |

The upstream tests are maintainer-reported evidence, not tests run by this project.
These findings do not establish a matching defect in pg-evidence. Prepared-query
checks must preserve the existing permission and citation invariants; they do not
replace the previously proposed cancellation tests. No pg_search installation or
performance comparison was performed.

## Other upstream checks

| Upstream | Observation on October 19 | Action |
|---|---|---|
| [PostgreSQL version policy](https://www.postgresql.org/support/versioning/), [18.6 notes](https://www.postgresql.org/docs/18/release-18-6.html) | The supported-version table still lists 18.6 for major 18; release date August 13. | Retain the tested pin. |
| [pgrx releases](https://github.com/pgcentralfoundation/pgrx/releases/tag/v0.19.3) | Latest returned release remains 0.19.3, September 27. | Retain the tested pin. |
| [pgvector tagged changelog](https://github.com/pgvector/pgvector/blob/v0.8.7/CHANGELOG.md) | Latest returned tag remains 0.8.7, October 1. | Retain the tested pin. |
| [pgvectorscale releases](https://github.com/timescale/pgvectorscale/releases/tag/0.9.1) | Latest returned release remains 0.9.1, September 4. | Previous comparator recommendation stands. |
| [Is Grep All You Need?](https://arxiv.org/abs/2605.15184v1), [PostgreSQL-V 2.0](https://arxiv.org/abs/2608.15994v1) | Submission histories still show v1 only, May 14 and August 17 respectively. | No revision-driven change to the previous interpretation. |

This is a bounded release and source review, not an audit of every unreleased
commit. Selected API metadata and immutable commit references are retained in
[sources.json](sources.json).

## Bounded prerequisite work: assess a filtered-search dataset

[Filtered Approximate Nearest Neighbor Search in Vector Databases: System Design
and Performance Analysis, 2602.11443v1](https://arxiv.org/html/2602.11443v1), published
**February 11**, studies metadata filtering and introduces MoReVec and a metric
relating global filter selectivity to the query's neighborhood. This is older work
newly assessed here. Its experiments use pgvector 0.8.1 with PostgreSQL 16 and
single-query, in-memory runs on a 256 GB Linux host. They do not establish current
PG18 performance or concurrent citation behavior. The useful implication is to
characterize query/filter correlation within our existing correlated/uncorrelated
matrix, rather than judge difficulty from eligible fraction alone. Do not import
the paper's tuning choices or speed claims into our evaluator.

Inspected the authors' repository at
[`aeaafd4c926adf3f5bd3e2649341a325076369e5`](https://github.com/aabylay/ANN-benchmark-HQ/tree/aeaafd4c926adf3f5bd3e2649341a325076369e5).
Its README links a Google Drive dataset folder and describes HDF5 embeddings,
filters and queries. The download script fetches that folder without checking an
expected file-hash manifest. The inspected README and loader do not establish
dataset-specific redistribution terms, fixed artifact hashes, or a mapping to our
immutable source bytes and citation ranges. Repository code licensing alone does
not resolve those dataset questions. No large download or upstream script was run.

Assessment: **candidate retrieval dataset, not an admitted benchmark fixture**.
Before admission, record artifact checksums and data provenance; determine how
source text and embeddings map to evidence IDs; recompute exact cosine neighbors
over the same visible current-version set as each arm. Version/history and citation
tests still need our own fixtures. A dataset admission or protocol revision must
be reviewed separately before optimization; current seeds and thresholds stand.

## Experiment track and verification

The product, SQL retrieval control and smoke runner exist. The baseline still
documents different envelope budgeting and serialization; no frozen semantic
fixture with exact eligible-set ground truth has been added. Therefore this pass
ran **zero optimization candidates, zero database benchmarks and zero holdout
evaluations**. This is a readiness decision, not a failed or successful performance
experiment. The smoke runner cannot answer the vector question above.

Main remains `f3a2ce82ce93f901458d7ac10fe4a582a82fcc2f`;
[PR #3](https://github.com/amaljithkuttamath/pg-evidence/pull/3) remains open at
`679f2e7cd851692c164934f8e4920858b07df9cb`. The local runtime previously verified
against that fix must not be labeled as a build of main.

[run-manifest.json](run-manifest.json) records unchanged evaluator hashes and the
host environment. Fresh checks: 59 benchmark tests passed; 28 packaging tests
passed and two skipped; protocol validation passed. Raw logs accompany this note.
No backend behavior changed or native build was rerun. Workflow authorization,
license, release and merge status are unchanged; no paid inference was used.
