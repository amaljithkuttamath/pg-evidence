# Initial research screening

Observed: 2026-10-07. Project reference: `d71f3b8`.
Scope: the original paper's abstract and primary upstream documentation.
This is source screening, not a full literature review or a replication.
No product benchmark was run: the extension and matched SQL baseline do not exist yet.

## What the sources establish

| Source | Observation | Implication for this project (inference) |
|---|---|---|
| [Is Grep All You Need?, v1](https://arxiv.org/abs/2605.15184v1) | The abstract reports a 116-question LongMemEval sample, multiple agent harnesses, and inline versus file-based results. Grep generally wins its first experiment, while harness and tool presentation affect outcomes. | Separate retrieval choice from output presentation and composition. This paper does not establish a universal advantage for either semantic retrieval or our proposed layer. |
| [pgvector, pinned README](https://github.com/pgvector/pgvector/blob/f37c13f68b57d2c3472b2214fbcff699d6d34876/README.md#iterative-index-scans) | Approximate retrieval can lose results after filtering. Iterative scans continue searching until enough results or configured limits are reached. | Compare filtered ANN against exact results over the same eligible rows. Record shortages, recall and resource limits together with latency. |
| [pgvectorscale, pinned README](https://github.com/timescale/pgvectorscale/blob/015ffc34c20e4d6f0ac822b08d7b94d84fee340a/README.md#filtered-vector-search) | StreamingDiskANN supports label filtering; arbitrary predicates use post-filtering. The implementation uses Rust and pgrx. | Include it as a vector component comparator when compatible. Our tags do not by themselves constitute a new search-index contribution. Verify label and predicate semantics before comparing. |
| [ParadeDB, pinned README](https://github.com/paradedb/paradedb/blob/dc5b7834bf5bb50be13ad3a84a8b48f5a93649d7/README.md#what-is-paradedb) | The project documents BM25, vector and hybrid search, filters and joins inside PostgreSQL. | Search in PostgreSQL is already an established capability. Evaluate citation lifecycle and agent-facing composition separately from search relevance. |
| [autoresearch, pinned README](https://github.com/karpathy/autoresearch/blob/228791fb499afffb54b46200aca536f79142f117/README.md) | The training experiment constrains candidate edits and evaluates them with a fixed setup and bounded training runs. | Adopt the separation of candidate and evaluator, with database correctness gates and workload-specific budgets. Training results provide no evidence about database performance. |

Upstream capability descriptions are not independently reproduced here. No vendor
speedup is used as evidence for pg-evidence. Pinned revisions identify inspected
documentation, not selected dependency versions or compatibility guarantees.

## Experiments to implement

These are development hypotheses. They do not revise the frozen benchmark protocol.

| ID | Question and comparison | Measurements | Prerequisite |
|---|---|---|---|
| E1 | Can a citation to version A resolve the same bytes after publishing B? Compare the extension with an ordinary SQL implementation of the same contract. | Exact bytes, version identity, invalid-span rejection, authorization, retry and restore behavior. Any incorrect result rejects the candidate. | Evidence schema and matched SQL baseline. |
| E2 | Does bounded composition reduce agent cost while preserving answer quality? Compare separate calls with composed calls, keeping retrieval and data fixed; examine inline and file-based output independently. | Total model token usage, tool calls, output bytes, wall time, answer quality and citation correctness. File output still incurs tokens when read. | Working operations, agent adapter, tokenizer accounting and approved evaluation budget. |
| E3 | What happens to filtered retrieval as irrelevant history grows? Compare exact pgvector search, tuned HNSW with iterative scans, and compatible pgvectorscale configurations. | Recall against the eligible exact set, returned-count shortages, p50/p95 latency, memory, index size and build time. | Fixed fixtures, filter semantics, tuned baselines and controlled benchmark host. |
| E4 | Does the layer add material overhead? Compare identical operations and returned evidence using matched SQL, then compare search components separately with PostgreSQL full-text search and compatible pg_search. | Latency distribution, throughput, allocation/memory observations and result equivalence where semantics match. Relevance comparisons remain separate when ranking differs. | Executable product and SQL paths, dataset manifest and repeatable runner. |

## Next decision

Build E1 first. Stable citation semantics provide a concrete acceptance test for
the layer. Then implement the matched operations needed for E2 before spending
money on live agent trials. The [benchmark method](../benchmark-method.md) remains
the authority for final claim thresholds and holdout isolation.

The paper's full methodology, benchmark data permissions, comparator build
compatibility, and the exact OpenWiki project intended in the original discussion
still need verification. They are not treated as established evidence here.
