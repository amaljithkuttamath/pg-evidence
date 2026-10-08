# Matched SQL baseline

The control arm for docs/testing-benchmark-plan.md §2: plain PostgreSQL SQL with
built-in full-text search, pgvector and adjacency tables. It never calls the
`pg_evidence` extension, so a comparison is never the extension against itself.

| File | Purpose |
|---|---|
| `schema.sql` | Canonical corpus DDL, statement for statement the same as `evidence.init_collection` (src/ddl.rs): identical tables, constraints, generated `tsvector`, GIN, HNSW and B-tree indexes, default limits |
| `wrapper.sql` | Session prepared statements: ingest, publish, tag, link, literal, lexical, lexical→one-hop neighbors→union, resolve; `attach`/`semantic` when `dims` is set |

```sh
psql -v corpus=docs_sql -f baseline/schema.sql
psql -v corpus=docs_sql -f baseline/wrapper.sql \
     -c "EXECUTE baseline_literal('needle', 10, 512, 65536)"
# With embeddings (pgvector installed in schema public):
psql -v corpus=docs_vec -v vector_schema=public -v dims=384 -f baseline/schema.sql
```

Prepared statements live for one session: run `wrapper.sql` and the `EXECUTE`
statements in the same `psql` session or script. Run the baseline under the same
`statement_timeout` as the extension arm.

## Matched semantics

- Current evidence only for search modes; same ordering (literal: path,
  start_byte, evidence_id; lexical: `ts_rank_cd` desc, evidence_id; semantic:
  cosine distance through the same HNSW index).
- `LIMIT k+1` to detect truncation; excerpts are the longest character prefix of
  at most `excerpt_bytes` UTF-8 bytes; whole results are dropped from the end to
  fit `max_response_bytes`.
- One-hop neighbors in both directions with the same edge ordering and limits;
  union deduplicates with seeds first.
- Resolve recomputes the source digest and compares the span with its byte slice.

## Known differences (not hidden)

- Budget: the baseline reserves a fixed 256 bytes for the envelope instead of
  the extension's exact search, so it may keep fewer results near the limit; a
  budget below 256 returns NULL rather than SQLSTATE 54000.
- Ingest: the baseline does not implement ingestion-key replay or request
  digests (`request_sha256` holds the key's digest); it is a retrieval control,
  not a retry-semantics reference. Publication with a stale base returns
  `{"revision": null}` instead of raising 55000.
- Response keys match the extension's result objects; the envelope carries only
  `results` and `truncation`.

Ties in rank or distance may order differently between arms because evidence IDs
are generated independently; compare tied results as sets.

This is a **development correctness control**, not yet a performance-equivalent
implementation of every API contract. Its `json_build_object` output includes
whitespace that the extension omits, so near-budget result counts can differ even
beyond the fixed envelope reserve. Its UTF-8 excerpt cut checks at most four byte positions
in a bounded prefix. The current differential tests compare retrieval content and
schema on small fixtures, not all boundary budgets or filtered ANN recall.
