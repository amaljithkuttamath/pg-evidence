# Testing and benchmark plan

Draft protocol, 2026-10-07. No implementation or benchmark results exist. This
document specifies experiments to incorporate into Task 1's frozen protocol;
proposed thresholds are not achieved results. It complements the approved proposal
and the independent review, and does not resolve the remaining SQL contracts.

## What we must establish

1. Retained citations identify the same exact evidence through updates and restore.
2. Retrieval returns useful evidence under filters and accumulated version history.
3. The layer has a measured, acceptable database and memory cost.
4. Composed tools reduce agent context at an explicitly measured quality level.

Each claim has its own experiment. A fast vector query cannot establish citation
integrity, and fewer tool calls cannot establish lower total token usage.

## 1. Correctness suite: required before release

Use a small independent reference model of assets, versions, spans and visibility.
Generate mutation sequences against both that model and a real PostgreSQL database.
Compare the observable state after each committed operation. Test with actual roles
and multiple connections, not a mocked database connection.

| Area | Required cases | Pass condition |
|---|---|---|
| Exact evidence | Multibyte characters, combining characters, CRLF, empty/reversed/out-of-range spans, invalid source input | Every accepted selector resolves exact retained bytes; invalid input has a documented error |
| Version lifecycle | Stage/publish A, stage/publish B, rename, retire, purge | A remains resolvable after B and retirement; purge produces the declared tombstone, never replacement content |
| Retry safety | Lost response after commit, duplicate key, conflicting key, concurrent identical retries | One logical mutation; repeat returns the contractually correct result |
| Concurrency | Two publishers, annotation plus publish, reciprocal links, read after write, publish during a plan | No mixed snapshot or unintended overwrite; conflicts and retries follow the contract |
| Authorization | Reader, writer, purger, owner; direct SQL attempts; hostile search_path and schema names | No unauthorized evidence or metadata; immutability holds within the declared trust boundary |
| Plans and limits | Cycles, forward references, deep JSON, huge strings, high-degree nodes, escaped output, cancellation | Rejection or valid bounded response; cancellation really ends backend work |
| Derived data | Embedding readiness, stale representation, dimension/model mismatch, purge of all derived copies | Correct visibility and readiness; no stale representation presented as current |
| Persistence | Transaction abort, killed backend, isolated server crash/restart, dump/restore with roles | Only permitted committed states survive; evidence bytes, IDs and grants match |
| Packaging | Build exact source archive, install binary on clean Linux/PG18, extension dependency failure, removal behavior | Reproducible activation and useful query; documented failure/rollback behavior |

Use pure Rust unit/property tests for span arithmetic and plan validation, plus
bounded fuzz runs on those parsers. Keep minimized failing seeds in regression
fixtures. Use pgrx database tests where the pinned version supports them, and an
external Python/psycopg harness for concurrent sessions, processes and restore.
The SQL baseline is also a differential oracle for exact retrieval and graph
results, but is not the sole oracle: shared schema mistakes can affect both paths.

Mandatory gate: no known unresolved violation of the declared invariants and no
unexpected failure in the release suite. This is evidence from tests, not a proof
of universal correctness. Upgrade testing becomes mandatory when a prior released
schema/version exists. Do not invent a historical release just to pass that check.

## 2. Comparators and what each answers

| Comparator | Experiment | Role |
|---|---|---|
| PostgreSQL SQL + built-in full-text search + pgvector + adjacency tables | Same corpus, schema, indexes, operations and result encoding as our layer | Required primary control; measures extension overhead/value |
| Thin external wrapper over that SQL | Direct calls and composed plans | Required agent control and round-trip comparison |
| pg-evidence | Direct calls and composed plans | System under test |
| Files + ripgrep + exact file reads | Literal lookup and repository navigation, with identical allowed source data | Required agent workflow baseline |
| pgvector exact scan and HNSW | Vector accuracy/latency curves with metadata filters | Required vector ground truth and index baseline |
| ParadeDB pg_search | Ranked text retrieval and filtered top-k | First external text comparison |
| pgvectorscale StreamingDiskANN | Vector accuracy, memory, build/update cost and filtering | External vector-scale comparison |
| pg_textsearch | BM25 ranked text retrieval | Optional second lexical comparator |

Upstream capabilities: [pgvector](https://github.com/pgvector/pgvector) supports
exact/approximate search; [ParadeDB](https://github.com/paradedb/paradedb) maintains
pg_search; [pgvectorscale](https://github.com/timescale/pgvectorscale) adds DiskANN
indexing to pgvector data; [pg_textsearch](https://github.com/timescale/pg_textsearch)
provides BM25. Pin and install actual compatible releases before including results.
Their published benchmark numbers are not results on our workload.

pgvector is part of our proposed implementation. Faster results from its index
must be attributed to the index, not to pg-evidence. A better result from another
index is useful evidence about a future backend choice, not a reason to build a
new index in v0.1.

Run two kinds of comparison:

- **Controlled component comparison:** SQL versus pg-evidence uses identical
  indexes, vectors, data, permissions, budgets, driver and serialization. Database
  time and client time are measured separately.
- **Alternative stack comparison:** allow each search extension its native index
  and documented tuning. Keep the corpus, task, machine/resource cap, query split
  and tuning effort comparable. Record tokenizer/analyzer differences; compare
  relevance/latency trade-offs rather than pretending ranking semantics match.

For version/citation tasks, give alternative retrieval stacks the same transparent
SQL provenance adapter and count its costs. Publish both the native capability
matrix and the full-stack results. A missing native citation API is “not provided”,
not zero retrieval quality. One-hop graph lookup compares first against indexed SQL
joins; a general graph extension is unnecessary for that operation.

## 3. Database workload matrix

Run on the declared Linux x86-64/PG18 target. Publish CPU model/core allocation,
RAM, storage, OS/kernel, PostgreSQL settings, versions, compiler flags, client
placement and container limits. Keep noisy development-machine timings out of
headline results.

| Dimension | Initial matrix |
|---|---|
| Current evidence spans | 100,000 and 1,000,000 |
| Retained history | 1, 5 and 20 versions per selected asset; report total stored spans separately |
| Concurrency | 1, 8 and 32 clients, subject to recorded host capacity |
| Eligible fraction under filters | 100%, 10%, 1%, 0.1%; correlated and uncorrelated vector/metadata cases |
| Retrieval | Literal, lexical, semantic, tag/path filtering; regex only if retained in scope |
| Composition | Search → one-hop neighbors → union/deduplicate → render |
| Writes | Ingest, embedding attach, publish, annotate, retire, purge; read-only and mixed read/write load |
| Graph shape | Sparse and skewed degrees; dedicated 10,000-edge adversarial node |
| Cache state | Warm steady-state and explicitly documented cold-start experiments |
| Memory | A normal cap and a cap below measured index-plus-corpus working set, fixed across comparators |

Do not run the full Cartesian product. Freeze a representative core matrix, then
separate history, selectivity, skew and memory-pressure sweeps. This preserves
coverage while keeping the experiment affordable. Random repeated text is suitable
for storage stress only; it cannot establish semantic quality.

Measure p50/p95/p99 latency, successful operations/sec, failures/timeouts, returned
bytes, CPU time, peak and steady-state memory, corpus/index size, build duration,
ingest/update throughput, WAL and recovery time. Record current data and historical
storage separately. Monitor PostgreSQL shared memory and backend-private memory;
do not sum process RSS as private usage. Keep raw cgroup and host-cache measurements.

Use [pgbench custom scripts](https://www.postgresql.org/docs/18/pgbench.html) for
database loads and a client harness for full requests. Begin with 30-second warmup,
120-second measurement and five independent runs as a pilot schedule. Extend
measurement if it provides too few observations or unstable tails; freeze the
final schedule before held-out measurement. Report uncertainty and sample counts.
Also use scheduled arrival-rate sweeps to expose queueing and saturation; report
late/skipped/failed requests rather than measuring only completed requests.

Alternate comparator order and start from equivalent dataset snapshots. Run query
plan diagnostics separately from timed headline runs. A database restart does not
by itself clear the operating-system page cache: state the actual cache procedure.
Run a sustained mixed workload to check memory growth, version accumulation and
maintenance effects; record autovacuum/checkpoint behavior and the run duration.

## 4. Retrieval quality

For ANN, compute exact nearest neighbors over the same eligible current evidence
under each query's filters. Define recall@k as overlap with that exact top-k, with
tie handling and queries with fewer than k eligible rows explicitly handled.
Report returned-result counts as well as recall. Measure latency/memory at common
recall targets such as 95% and 99%; do not call a lower-recall configuration faster
without disclosing the trade-off. Targets are protocol choices, not measured facts.

For lexical and semantic relevance, use human relevance labels, Recall@k and
nDCG@10. These are distinct from ANN recall: perfect nearest-neighbor retrieval can
still retrieve irrelevant text if the embeddings are poor.

[BEIR](https://github.com/beir-cellar/beir) provides public retrieval evaluation
datasets including SciFact. Use a documented public slice as an external relevance
check. The original [SciFact](https://github.com/allenai/scifact) project also has
claim/evidence verification material; confirm which split has public labels.
Neither validates our byte identities, updates or backup behavior.

The main product corpus is a manifest of real, licensed Markdown/docs/source
repositories at fixed commits, with recorded file bytes and independently checked
version/citation fixtures. Freeze embeddings (model, dimensions, input formatting,
normalization and file hashes) once and reuse them for matched comparisons. Split
development, pilot and final holdout by source groups; detect duplicates across
groups. Pin chunking and retain mappings from chunks to original source offsets.

## 5. Agent experiment and primary claims

Run the same agent/model configuration over these four cells:

| Backend | Direct calls | Composed plan |
|---|---|---|
| SQL wrapper | S-D | S-C |
| pg-evidence | E-D | E-C |

Composition's primary token contrast is **E-C versus E-D**, with **S-C versus S-D**
as the replication/control. Extension execution overhead is **E-C versus S-C**
(and E-D versus S-D for individual calls). Files/ripgrep is an additional workflow
comparison and cannot alone attribute savings to extension execution.

Pilot approximately 100 manually checked questions covering exact lookup,
paraphrase, filtered retrieval, one-hop gathering, conflicting/current versions,
historical citations and unanswerable questions. Freeze an answer-support rubric.
Keep expected supporting evidence independent of the retrieval implementation.
Repeat paired trials and retain all attempts, not just successful answers.

Measure supported-answer rate, citation resolvability, citation precision and
coverage, evidence recall, all model input/output tokens, cached usage, calls,
wall-clock time and cost. Include schemas, generated code, failed calls, retries,
expansion and verification. Count any summarization/extra model use. Report ingest
and embedding costs separately with a stated amortization assumption. A code/tool
execution wrapper gets identical sandbox capabilities in each relevant arm.

Proposed primary token estimand: ratio of mean total model tokens per attempted
question, averaging repetitions within question and then giving each question equal
weight. Report supported-answer quality separately; early failures cannot qualify
as token savings. Stratify or define weighting by corpus before the final run.

The earlier product target becomes: token-ratio upper one-sided 95% bound <= 0.75,
AND supported-answer-rate difference lower one-sided 95% bound > -0.03, for the
named primary composition contrast. Predeclare the joint decision rule and any
multiplicity adjustment for additional claims. Cluster uncertainty according to
the sampling design; few repositories cannot justify a broad cross-repository
claim. Determine the final fresh holdout size from pilot discordance/variance,
cluster structure and power. The 100-question pilot is not proof of a 3-point margin.

Human reviewers should be blinded to system identity, with disagreements
adjudicated. Automated byte/ID checks establish resolution; they do not establish
that an excerpt supports a generated assertion. Report those metrics separately.

## 6. Release gates and evidence package

- **Functionality:** pass the exact-evidence, permission, concurrency, cancellation,
  persistence and clean-package suites; no known unresolved integrity failures.
- **Retrieval:** match the exact-mode oracle; disclose ANN accuracy and failures
  under every declared slice. Freeze the default ANN quality target after the pilot
  and meet it in final evaluation; 95% recall@10 is an initial candidate.
- **Performance:** choose numeric latency, memory and acceptable-overhead budgets
  on a named workload/machine after the pilot, before the final run. A violated
  frozen budget blocks that performance promise. There is no justified universal
  millisecond or RAM limit yet. Do not move thresholds after seeing final results.
- **Agent evidence:** publish actual live evaluation and adjudication results. A
  failed/inconclusive efficiency target can still accompany a functional release;
  the 25% efficiency claim requires both statistical conditions above.
- **Reproducibility:** ship runner, exact package/source checksums, dataset manifests,
  seeds, configuration, licenses, raw machine-readable runs and analysis commands.
  Replicate the package and report generation from a clean environment.

Run fast unit/property and focused PostgreSQL tests on changes; use dedicated
scheduled runs for fuzzing, recovery and sustained workloads; run the full pinned
matrix for release candidates. Expensive external-stack comparisons follow the
matched SQL baseline. A blocked comparator is recorded as unmeasured with its
reason, not filled in from vendor claims. Until compatible packages are tested,
do not advertise the external comparison suite as operational.

The first implementable acceptance scenario is:

1. Ingest and publish source A; capture a citation and its expected bytes.
2. Publish edited source B; current retrieval returns B and the old citation
   continues to resolve A.
3. Repeat a committed request after simulating a lost response; no duplicate.
4. Race two publishers; observe only the contractually permitted outcomes.
5. Dump and restore into a clean server; repeat byte and permission assertions.

Build that scenario and the matched SQL baseline before expanding the retrieval
surface. No test runner or result claimed in this document exists yet.
