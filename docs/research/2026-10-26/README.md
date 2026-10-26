# Index layout, cancellation and label-aware compression — 2026-10-26

Decision: use **pg_search 0.26.3** for the next comparator qualification, record
index-build settings and resulting segment layout, and test cancellation during
active operators. Keep our PostgreSQL/pgvector implementation. Label-aware
compression is a research direction, not a validated product option.

All sources were inspected October 26. This extends the
[October 19 record](../2026-10-19/README.md); it changes no evaluator or product code.

## Newly released comparator changes

[ParadeDB 0.26.3](https://github.com/paradedb/paradedb/releases/tag/v0.26.3) was
published **October 22, 23:28:33 UTC**, at commit
`b3421945b45fdd177272dc9a204c7d47a7cdb527` after resolving its annotated tag.

| Primary evidence | Date merged | Implication for our comparison |
|---|---|---|
| [#6874](https://github.com/paradedb/paradedb/pull/6874), [backport #6882](https://github.com/paradedb/paradedb/pull/6882) | October 17 | Automatic segment targets now depend on query-worker limits and partition-key count; explicit/global overrides retain precedence. Record both build-time settings and actual segment layout. Changing a worker setting can affect the index built, not just query execution. The upstream change did not rerun full dataset benchmarks. |
| [#6901](https://github.com/paradedb/paradedb/pull/6901), [backport #6920](https://github.com/paradedb/paradedb/pull/6920) | October 19 / 22 | Removes the unconditional 1,000-document vector flush cap and converts eligible flat segments to IVF after worker build/merge. Record ingestion batch history, index size, build time and flat/IVF layout; rebuilding under the new version is a different index construction treatment. The source explicitly says it does not fix the underlying component-reader pin-retention issue. |
| [#6886](https://github.com/paradedb/paradedb/pull/6886), [backport #6964](https://github.com/paradedb/paradedb/pull/6964) | October 21 | Adds interrupt handling during DataFusion scans, term-dictionary walks and IVF training. Test cancellation and statement_timeout while work is active, preserve SQLSTATE and check session recovery. The source excludes some operators after scan exhaustion, such as a large sort: this is not blanket interruptibility evidence. |

These are upstream reports, not reproduced measurements. They neither establish
defects in pg-evidence nor justify a speed claim. Previous permission and cached
tenant-filter checks remain required. A future version comparison must distinguish
an unchanged index from a freshly rebuilt one rather than attribute all differences
to query code or compression.

## Papers: useful ideas and limits

[AiSAQ-in-Filtering, arXiv:2610.17627v1](https://arxiv.org/html/2610.17627v1),
published **October 13**, combines vector and label subvectors in product
quantization using a weighted fusion distance. Compressed neighbor representations
reside on SSD; full-precision labels remain available for reranking. This is not
TurboQuant or a drop-in pgvector encoding.

The evaluation uses SIFT1M and KILT E5 1M with synthetic labels and single-label
queries. It reports disk-I/O counts as a speed proxy because fusion computation
is not optimized. Recall is worse on uniformly labeled KILT in one comparison;
the reported 78-fold memory ratio comes from a particular SIFT uniform-label case.
It is not an end-to-end PostgreSQL memory result.

Our inference: label-aware candidate routing is worth tracking, but exact tag,
permission and current-version eligibility must remain authoritative in SQL.
Evaluate total resident memory, OS cache, disk size and measured latency together;
include tag changes and index maintenance before adopting such a representation.
The original source bytes and citation verification must remain unchanged.

[VADER, arXiv:2609.38053v2](https://arxiv.org/html/2609.38053v2), revised
**September 30**, uses learned recall prediction to terminate filtered search.
It highlights that filling k result slots is not evidence of retrieving the true
nearest eligible neighbors. The paper evaluates per-query recall distributions
and the fraction of queries below the target, alongside averages.

Our inference: the future filtered-recall report should expose query-level
shortfalls within the already planned selectivity/correlation matrix. This does
not add an acceptance threshold or authorize predictor training. The reported
results are not guarantees for our data, and the current extension provides no
internal traversal features for this predictor. No model inference was added.

## Release sweep and experiment readiness

The [PostgreSQL supported-version table](https://www.postgresql.org/support/versioning/)
still lists 18.6; the pgrx release, pgvector tag and pgvectorscale release queries
still return 0.19.3, 0.8.7 and 0.9.1 respectively. Their stable links and dates are
retained in [sources.json](sources.json). The original grep, PostgreSQL-V and
MoReVec paper submission histories still show v1 only. This was a bounded release
review and paper search, not an exhaustive audit of unreleased commits.

Main and the open fix PR remain unchanged. The SQL baseline still differs in
envelope budgeting and serialization; no frozen semantic dataset and exact
eligible-set ground truth have been added. No optimization candidate, database
benchmark or holdout ran. Repeating the small literal smoke would not answer the
new compression or vector-layout questions. The next experiment still requires
separate prerequisite work, followed by a frozen evaluator and correctness checks.

[run-manifest.json](run-manifest.json) records base commits, host metadata and
unchanged evaluation-file hashes. Host verification logs accompany this note:
59 benchmark tests passed, 28 packaging tests passed with two skipped, and
protocol validation passed. No native build or database suite was rerun for this
documentation-only change. No release, automatic merge, workflow authorization,
license change or paid model/API spending occurred.
