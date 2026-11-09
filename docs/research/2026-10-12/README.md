# Upstream findings and experiment readiness — 2026-10-12

Decision: retain the existing PostgreSQL architecture and dependency pins. Use
pg_search 0.26.1 as the minimum version for a future 0.26-series comparison, and
check permission filtering and cancellation before measuring it. No optimization
candidate was run: vector evaluation inputs and fully equivalent response budgets
are still missing. This is research evidence, not a product performance result.

## Sources and implications

All sources were inspected on 2026-10-12. Dates below are publication dates, not
discovery dates. Tagged upstream documents and versioned papers identify the
material reviewed; this is a bounded review, not an exhaustive upstream audit.

| Source | Date / version | Finding and implication |
|---|---|---|
| [PostgreSQL release notes](https://www.postgresql.org/docs/18/release-18-6.html) | 2026-08-13, 18.6 | The project already pins this maintenance version. Retain it for reproducibility; this review does not certify later major-version compatibility. |
| [pgrx release](https://github.com/pgcentralfoundation/pgrx/releases/tag/v0.19.3) | 2026-09-27, 0.19.3 | Already pinned. Includes interrupt-handling and varlena safety fixes, plus PG19 beta support. Beta support alone does not justify changing our tested PG18 target. |
| [pgvector changelog](https://github.com/pgvector/pgvector/blob/v0.8.7/CHANGELOG.md) | 2026-10-01, 0.8.7 | Already pinned. Fixes an IVFFlat build buffer overflow and empty-input averaging. Keep the existing pin; these are not measured improvements to our HNSW workload. |
| [pg_search release](https://github.com/paradedb/paradedb/releases/tag/v0.26.1) | 2026-10-08, 0.26.1 | New since the previous project research. Includes RLS predicate-ordering and heap-filter interrupt fixes. Benchmark the corrected version, with security and cancellation checks first. |
| [pgvectorscale release](https://github.com/timescale/pgvectorscale/releases/tag/0.9.1) | 2026-09-04, 0.9.1 | Earlier release, newly examined here. Hardens pgvector schema/type resolution and dimension/datum validation. Use at least this version when introducing the comparator; neither installation nor migration was performed. |

GitHub API release metadata is retained in
[upstream-release-metadata.json](upstream-release-metadata.json). pgvector's empty
releases API response is not an absence-of-updates finding; its tagged changelog
provides the version evidence. The pg_search 0.26.1 annotated tag resolves to
commit `529c77e9dffe4bd86b6b7971c019bb26b8eadbfa`.

### Permission filters and cancellation are comparison gates

ParadeDB's [original RLS fix #6614](https://github.com/paradedb/paradedb/pull/6614)
describes predicates being evaluated before row-security checks, potentially
exposing rejected-row data through errors. Its
[0.26 backport #6679](https://github.com/paradedb/paradedb/pull/6679) was merged on
October 5. This is evidence about that upstream implementation, not a finding
that pg-evidence has the same defect.

Proposed follow-up: run each comparison arm under the same restricted reader role.
Compare visible IDs, counts, errors and LIMIT behavior with a plain PostgreSQL
control, including predicates that fail on denied rows. Capture actual query plans
so a fallback plan is not mistaken for custom-scan performance. Current grants
tests alone do not establish those comparator properties.

The [interrupt fix #6670](https://github.com/paradedb/paradedb/pull/6670), also
merged October 5, covers long runs of rejected heap-filter candidates. Proposed
follow-up: exercise an active scan with many rejected candidates under
statement_timeout and explicit cancellation. Verify cancellation is reported and
the connection can be reused. The [backport #6677](https://github.com/paradedb/paradedb/pull/6677)
is in 0.26.1. A timeout before an operator starts does not establish interrupt
responsiveness inside that operator.

### Vector filtering before compression

[pgvector's tagged iterative-scan documentation](https://github.com/pgvector/pgvector/blob/v0.8.7/README.md#iterative-index-scans)
describes expanding ANN scans when filtering leaves too few results, subject to
scan limits. The feature dates to 0.8.0; it is not new this week.

Candidate hypothesis, not a result: strict iterative HNSW scanning may reduce
underfill when current-version and tag filters reject many candidates. Before
testing, freeze embeddings, queries, exact eligible-set cosine ground truth,
history distributions, limits and recall thresholds. Record plans, result counts,
recall, latency and memory. More scanning can increase cost; it does not guarantee
exact nearest neighbors. See the existing [compression plan](../../vector-compression.md)
for subsequent float32/halfvec/binary comparisons. TurboQuant remains unimplemented.

### Architecture evidence: PostgreSQL-V 2.0

[Building An Integrated Vector Database System in PostgreSQL,
arXiv:2608.15994v1](https://arxiv.org/html/2608.15994v1), published August 17, 2026,
describes a decoupled index service, immutable segments, WAL-backed recovery
metadata and separate segment replication. Sections 4–6 show the concurrency and
recovery machinery required when bypassing ordinary index pages. Read-only mmap
reduces cold-start unavailability; it is not evidence of reduced total memory.

Section 7 uses large Linux servers. Dynamic throughput and recall are measured
with different concurrency schedules because mixed updates complicate ground
truth. Reported speedups are not transferable to our ARM development host or
citation API. We have not reproduced them.

Our inference: keep PostgreSQL and pgvector for this release. A separate index
service would add substantial recovery, replication and memory-accounting work.
If later justified by profiles, measure crash-to-searchable time separately from
recovery time, include all service memory, and check snapshot-aligned recall.
This is a future architecture option, not an endorsed rewrite.

## Experiment track: readiness decision

Reviewed repository main commit
`f3a2ce82ce93f901458d7ac10fe4a582a82fcc2f`. The existing paired runner is executable,
but [baseline limitations](../../../baseline/README.md) prevent general
performance-equivalence claims. The literal smoke compares result objects on a
small ASCII fixture; it excludes the complete response envelope and has no
embeddings, retained-version pressure or permission filters. It cannot support
ANN recall, memory or token-efficiency conclusions.

The local fixed runtime belongs to [PR #3](https://github.com/amaljithkuttamath/pg-evidence/pull/3),
commit `679f2e7cd851692c164934f8e4920858b07df9cb`, while main still predates those
lifecycle fixes. Do not label tests of that image as tests of main.

This session ran **zero optimization candidates, zero database benchmarks and
zero holdout evaluations**. No failed performance results exist for this session
because no performance run started. Host verification logs are separate from
performance evidence. [run-manifest.json](run-manifest.json) records the base,
environment and raw-file hashes of unchanged evaluation files. The protocol
validator reports a separate canonical frozen-field hash; it is not the raw-file
SHA-256 recorded in the manifest.

Next prerequisite work, each separately reviewable before optimization:

1. Make the SQL control match the measured response contract, including boundary
   budgets, UTF-8 serialization and error behavior. Freeze the resulting evaluator.
2. Freeze semantic fixtures and exact eligible-set ground truth using the existing
   protocol's seeds and workload matrix. Do not tune acceptance thresholds.
3. Establish the restricted-role and active-operator cancellation gates above,
   then compare at most three candidates against the matched SQL control within
   the existing 45-minute experiment limit.

This PR changes research records only. It does not revise the evaluator, fixtures,
thresholds, dependency pins or product behavior. Workflow authorization and the
license decision remain unresolved; no release, merge or paid inference occurred.

## Verification

Fresh host checks on October 12: 59 benchmark tests passed; packaging ran 30 tests
with 28 passing and two skipped; protocol validation passed. Raw output is in
`bench-tests.log`, `packaging-tests.log` and `protocol-check.log` beside this note.
No database behavior changed, and no native build or database suite was rerun.
