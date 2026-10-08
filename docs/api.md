# pg-evidence v0.1 SQL/JSON API

Implements the [design contract](design.md). Verification status: see
"Verification" at the end. Nothing here is a performance claim.

```sql
evidence.init_collection(corpus text, config jsonb) RETURNS void
evidence.stage_version(corpus text, request jsonb)   RETURNS json
evidence.attach_embeddings(corpus text, request jsonb) RETURNS void
evidence.publish_version(corpus text, request jsonb) RETURNS json
evidence.retire(corpus text, request jsonb)          RETURNS json
evidence.annotate(corpus text, request jsonb)        RETURNS json
evidence.purge(corpus text, request jsonb)           RETURNS json
evidence.query(corpus text, request jsonb)           RETURNS json   -- STABLE
evidence.resolve(corpus text, evidence_id uuid)      RETURNS json   -- STABLE
```

All functions: `SECURITY INVOKER`, `STRICT` (a NULL argument returns NULL),
`PARALLEL UNSAFE`, `SET search_path = pg_catalog, pg_temp`. `query` and
`resolve` are `STABLE`, the others `VOLATILE`. Every function refuses a non-UTF8
database (`55000 non_utf8_database`) and a corpus whose `schema_version` is not 1
(`55000 unsupported_schema_version`, with a `hint`). An unknown corpus surfaces
PostgreSQL's own `42P01`. Every JSON object rejects unknown fields (`22023`).

## Conventions

- **UUIDs** are hyphenated strings, any case on input, lowercase on output.
- **Offsets** are UTF-8 byte offsets into the exact stored source, half-open
  `[start_byte, end_byte)`, and must fall on character boundaries.
- **Digests** are 64 lowercase hex digits: SHA-256 of the exact UTF-8 bytes.
- **Errors** carry the SQLSTATE in the table below and a JSON `DETAIL` object
  with at least `reason` (for example `{"reason":"revision_conflict",
  "current_revision":2,"expected_revision":1}`); validation errors add `field`,
  limit errors add `limit` and `max`.
- **Text fields** (`path`, `ingestion_key`, tags, `reason`, model names) must be
  non-empty, within their byte limit and free of control characters.

| SQLSTATE | `reason` values |
|---|---|
| `22023` | `invalid_request`, `source_sha256_mismatch`, `unknown_version`, `unknown_asset`, `unknown_evidence`, `evidence_not_in_version`, `semantic_not_configured`, `embeddings_not_configured` |
| `23505` | `ingestion_key_reused`, `embedding_conflict`; a current-path collision is PostgreSQL's own unique violation |
| `42501` | PostgreSQL privilege errors; `purger_required` (raised by `purge` before any read) |
| `42P06` | PostgreSQL: the corpus schema already exists |
| `54000` | `limit_exceeded`, `response_envelope_too_large` (query), `response_too_large` (resolve, mutations) |
| `55000` | `revision_conflict`, `version_purged`, `version_current`, `version_published`, `embeddings_missing`, `pgvector_missing`, `statement_timeout_unset`, `unsupported_schema_version`, `non_utf8_database`, `invalid_collection_config`, `relation_changed` (transient; retry) |
| `XX001` | `span_mismatch`, `digest_mismatch`, `source_missing`, `span_missing` |
| `22021`, `22P02`, `22P05`, `2201B`, `57014`, `40001`, `40P01` | Raised by PostgreSQL (invalid UTF-8, malformed JSON, `\u0000`, invalid regex, cancel/timeout, serialization, deadlock) and never converted |

A jsonb request nested deeper than 128 levels or with numbers outside the
double range cannot be represented by the parser and is `22023`.

## Limits

Configured per collection (`config.limits`; defaults, ceilings):

| Limit | Default | Ceiling | Applies to |
|---|---|---|---|
| `max_source_bytes` | 1 MiB | 64 MiB | staged source; `resolve` verification |
| `max_response_bytes` | 64 KiB | 16 MiB | every returned `json` value (see Budgets) |
| `max_candidates_per_operator` | 256 | 10,000 | `limit` of each plan node |
| `max_plan_nodes` | 32 | 256 | nodes per plan |
| `max_edges_returned` | 1,024 | 100,000 | sum of `max_edges` over neighbors nodes |

Fixed protective limits (`54000`, `limit` names in the detail): 65,536 spans per
version (`max_spans_per_version`); total span bytes at most 4 × `max_source_bytes`
(`max_span_bytes_total`, overlapping spans copy bytes); at most 1,048,576 vector
values (vectors × dimensions) per `attach_embeddings` call
(`max_embedding_values_per_call`); for `query`, 6 × output-node `limit` ×
`excerpt_bytes` at most 64 MiB (`max_excerpt_materialization_bytes`, checked
before any SQL runs). The last bounds the worst-case excerpt payload the single
evidence statement can build before Rust drops results for the response budget:
SQL `left()` counts characters (up to 4 UTF-8 bytes each) and JSON escaping
expands text, so 6 bytes per character is the bound used. It is not a limit on
process memory, RSS or total SQL work, and does not affect `stage_version` or
`resolve`; with the default excerpt (512 bytes) any `limit` up to 21,845 passes. Fixed validation limits (`22023`): path 1,024
bytes, ingestion key 256, purge reason 1,024, tag 128, 64 tags per call, model 256,
literal/regex/lexical text 1,024 bytes, 64 filter tags, 1,000 filter asset IDs,
2–16 union inputs, node IDs `^[a-z][a-z0-9_]{0,31}$`, relation kinds
`^[a-z][a-z0-9_]{0,62}$`. These are protective bounds, not measured capacities.
The request jsonb is fully parsed before validation; these limits bound the
copies and SQL work the extension creates, not PostgreSQL's own parsing.

### Budgets (clarification of the contract)

- `query`: results are dropped whole from the end until the exact returned text
  fits `max_response_bytes` (or the request's smaller `max_response_bytes`); JSON
  is never cut; if the envelope alone cannot fit: `54000 response_envelope_too_large`.
- `resolve` and all mutation responses cannot be shortened without losing
  meaning (an exact citation, the full list of new evidence IDs). They obey the
  same `max_response_bytes` by failing with `54000 response_too_large`; the
  error rolls back the call's writes. `stage_version` computes its response size
  before any write (IDs are fixed-length). To stage many spans, raise the
  collection's `max_response_bytes` (about 95 bytes per span) or split content
  into several assets.

## init_collection

```json
{"embedding_model": "text-embedding-x", "embedding_dimensions": 384,
 "text_search_config": "english", "limits": {"max_response_bytes": 131072}}
```

All keys optional. `embedding_model` and `embedding_dimensions` are both set
(dimensions 1–2000, requires pgvector: else `55000 pgvector_missing`) or both
null. `text_search_config` defaults to `simple`; an unqualified name resolves in
`pg_catalog`, a qualified one as `schema.name` (no dots inside names); it is
stored schema-qualified and fixed into the generated `tsvector` column. Corpus
names match `^[a-z][a-z0-9_]{0,47}$`, not `pg_*`, `public`, `evidence`,
`information_schema` or pgvector's schema. The caller owns the new schema and
tables (DDL: `src/ddl.rs`, mirrored in `baseline/schema.sql`). Grants are not
created: see [operations](operations.md).

## stage_version

```json
{"asset_id": "6f1c0a52-3b7e-4d0c-9a51-0c7d2b9e4a11", "path": "guide/retries.md",
 "source": "…exact text…", "source_sha256": "<sha256 of source bytes>",
 "spans": [{"start_byte": 0, "end_byte": 17}], "ingestion_key": "guide/retries.md@1",
 "expected_revision": 0}
```

Response (spans in request order):

```json
{"status":"staged","replayed":false,"version_id":"…","asset_id":"…","path":"guide/retries.md",
 "source_sha256":"…","byte_length":57,"base_revision":0,
 "spans":[{"evidence_id":"…","start_byte":0,"end_byte":17}]}
```

Steps follow the contract: validate; compute `request_sha256`; look up
`ingestion_key` first (same digest: return the stored IDs with
`"replayed": true`, no other check; different digest: `23505`); insert the asset
if absent (a new asset with `expected_revision` ≠ 0 is `55000
revision_conflict`, writing nothing); insert the version `ON CONFLICT
(ingestion_key) DO NOTHING` and on conflict re-apply the lookup; insert spans.
Empty `spans` is allowed. Spans must be distinct.

**Canonical request digest** (`request_sha256`): SHA-256 of the UTF-8 text

```
{"asset_id":"<lowercase uuid>","expected_revision":<n>,"path":<JSON string>,"source_sha256":"<hex>","spans":[[s,e],...]}
```

with no whitespace, spans in request order and the path escaped as by
serde_json. The ingestion key is the lookup key and is excluded; the source is
represented by its verified digest.

## attach_embeddings

```json
{"version_id": "…", "model": "text-embedding-x",
 "embeddings": [{"evidence_id": "…", "vector": [0.1, -0.2, 0.3]}]}
```

Returns void. Locks the asset row, then in order: purged version `55000
version_purged`; wrong model, dimension, non-finite (after float4 rounding) or
zero-norm vector, repeated or foreign evidence ID `22023`; a vector differing from
the stored float4 values `23505 embedding_conflict`; all already stored
identically: success with no write (also after publication); published version
needing a new vector `55000 version_published`; otherwise insert the missing
vectors. Only a collection with embedding configuration accepts this call
(`22023 embeddings_not_configured`). Vectors are computed by the client; the
backend never calls a model. Distance is cosine.

## publish_version

`{"version_id": "…"}` → `{"status":"published","replayed":false,"version_id":"…",
"asset_id":"…","revision":2,"path":"…","published_at":"…","published_by":"…","current":true}`

Locks the asset row. An already published version returns its record with
`"replayed": true` (and `current` telling whether it is still current). Then:
purged `55000 version_purged`; `base_revision` ≠ `content_revision` `55000
revision_conflict` (detail `current_revision`, `base_revision`); missing
embeddings when configured `55000 embeddings_missing` (detail `missing`).
Otherwise inserts the publication with revision `content_revision + 1`, then
moves the current pointer and path and clears `retired_at` in one `UPDATE`. A
current-path collision with another asset is `23505` and rolls back both writes.

## retire

`{"asset_id": "…", "expected_revision": 2}` →
`{"status":"retired","asset_id":"…","content_revision":3,"retired_at":"…"}`.
Mismatch: `55000 revision_conflict`. Retired versions remain resolvable.

## annotate

Discriminated by `action`:

| Request | Response |
|---|---|
| `{"action":"tag","asset_id":…,"tags":["a"],"expected_annotation_revision":0}` | `{"status":"annotated","action":"tag","asset_id":…,"annotation_revision":1,"added":["a"]}` |
| `{"action":"untag", … same fields}` | `… "removed":["a"]` |
| `{"action":"link","source_evidence_id":…,"target_evidence_id":…,"kind":"cites"}` | `{"status":"linked" or "existing","source_evidence_id":…,"kind":…,"target_evidence_id":…,"asserted_by":…,"asserted_at":…}` |
| `{"action":"unlink", … same fields}` | `{"status":"unlinked" or "absent", …}` |

`tag`/`untag` lock the asset, compare `annotation_revision` (`55000
revision_conflict`) and increment it, independently of `content_revision`; they
increment even when no tag changed. `added`/`removed` list actual changes.
`link` rejects self-links and unknown evidence (`22023`); cycles are allowed.
`link` inserts with `ON CONFLICT DO NOTHING` and, if no row was inserted, reads
the existing one. A concurrent `unlink` can commit between those statements; the
pair is then retried once, and if the relation vanishes again the call fails
with `55000 relation_changed` (detail `hint`). This is transient: retry the call.
No row lock or `UPDATE` grant is involved.

## purge

`{"version_id": "…", "reason": "privacy request 17"}` →
`{"status":"purged","replayed":false,"version_id":…,"asset_id":…,"purged_at":…,
"purged_by":…,"reason":…,"spans_purged":3,"embeddings_deleted":3}`

Requires `UPDATE (source)` on `versions` (`42501 purger_required`, checked
first). Locks the asset; an already purged version returns its tombstone with
`"replayed": true`; the current version is `55000 version_current`. Nulls the
source and span text (the `tsvector` follows), deletes the spans' embeddings and
inserts a tombstone, keeping IDs, offsets, path, digest, byte length,
publications and relations.

## query

```json
{
  "nodes": [
    {"id": "hits", "op": "lexical", "query": "retry semantics", "limit": 8,
     "filter": {"path_prefix": "guide/", "tags_all": ["v1"], "tags_any": ["a", "b"], "asset_ids": ["…"]}},
    {"id": "rx",   "op": "regex", "pattern": "^Retry", "case_insensitive": true},
    {"id": "lit",  "op": "literal", "text": "lost response"},
    {"id": "sem",  "op": "semantic", "model": "text-embedding-x", "vector": [0.1, 0.2, 0.3]},
    {"id": "near", "op": "neighbors", "from": "hits", "direction": "both",
     "kinds": ["cites"], "status": ["current", "historical"], "limit": 16, "max_edges": 64},
    {"id": "all",  "op": "union", "inputs": ["hits", "near"], "limit": 20}
  ],
  "output": "all",
  "excerpt_bytes": 400,
  "max_response_bytes": 32768
}
```

(Every node must feed `output`, so the example's `rx`, `lit` and `sem` would be
rejected unless they were union inputs.) Rules: 1 to `max_plan_nodes` nodes;
inputs must name earlier nodes, so the plan is a DAG by construction; unused
nodes, duplicate IDs and unknown ops or fields are `22023`. `limit` defaults to
10 (union: sum of its inputs), maximum `max_candidates_per_operator` (`54000`
above). Neighbors nodes without `max_edges` share what is left of
`max_edges_returned`. `excerpt_bytes` defaults to 512 and may not exceed the
response budget. `query` refuses to run when `statement_timeout` is 0 (`55000
statement_timeout_unset`); the timeout is the only bound on database work.

| op | Matches | Order |
|---|---|---|
| `literal` | `strpos(span text, text) > 0`, case-sensitive | path, start_byte, evidence_id |
| `regex` | PostgreSQL `~` (`~*` if `case_insensitive`); no index or latency guarantee | path, start_byte, evidence_id |
| `lexical` | `tsv @@ websearch_to_tsquery` (`syntax`: `websearch`, `plain`, `phrase`) with the collection's configuration | `ts_rank_cd` desc, evidence_id |
| `semantic` | pgvector cosine distance through the HNSW index; `approximate` | distance, evidence_id |
| `neighbors` | one hop over relations from the `from` node's rows; `direction` `out`/`in`/`both`; optional `kinds`, endpoint `status` filter | first edge order |
| `union` | deduplicated rows of its inputs | input order, then rank |

Search ops return only current evidence; filters apply to them. Neighbor
endpoints may have any status; purged endpoints have `excerpt: null`.

Response:

```json
{"results":[{"evidence_id":"…","version_id":"…","asset_id":"…","path":"guide/retries.md",
   "start_byte":18,"end_byte":57,"status":"current","mode":"lexical",
   "excerpt":"a lost response replays the stored IDs.","excerpt_truncated":false,"rank":0.1}],
 "edges":[{"source_evidence_id":"…","kind":"cites","target_evidence_id":"…"}],
 "truncation":{"requested":20,"returned":1,"truncated":false,"underfilled":true,
   "dropped_for_budget":0,"edges_truncated":false,"excerpts_truncated":false},
 "nodes":[{"id":"hits","op":"lexical","requested":8,"returned":1,"truncated":false,"underfilled":true}],
 "readiness":{"literal":"ready","regex":"ready","lexical":"ready","semantic":"not_configured"},
 "approximate":false,"complete":true}
```

- `status`: `current`, `historical` (published, superseded), `staged`, `retired`
  (published, asset retired), `purged` (takes precedence).
- `mode` is the op that first produced the row; lexical rows carry `rank`,
  semantic rows `distance` and `"approximate": true`.
- Excerpts are the longest prefix of the span text of at most `excerpt_bytes`
  UTF-8 bytes, cut on a character boundary.
- `edges` lists relations traversed by neighbors nodes whose produced endpoint
  is in `results`.
- A node is `truncated` when more rows matched than its limit (or its edges
  exceeded `max_edges`); `underfilled` when it returned fewer than requested.
- `complete` is true only if no node was truncated, no result was dropped for
  the budget, no excerpt was cut, no edge budget was hit and no approximate
  (semantic) node took part. No truncated or approximate result is labelled complete.
- `readiness.semantic`: `ready`, `not_configured`, or `unavailable` (configured
  but pgvector is not installed; a clarification beyond the contract's two values).

### Snapshot semantics (clarification of "one statement per call")

`query` issues exactly two SQL statements: first a **metadata read** of the
corpus's one-row `collection_config` (limits, text-search configuration,
embedding settings), pgvector's schema from `pg_extension` and the current
`statement_timeout`; then **one evidence statement**. All evidence (spans,
versions, assets, publications, tombstones, tags, relations, embeddings) — every
retrieval, filter, traversal and excerpt — is read by that single statement, so
it observes one snapshot at any isolation level. The metadata read precedes it
and may use an earlier snapshot at `READ COMMITTED`; `collection_config` is
writable only by the corpus owner, who is trusted. `resolve` likewise performs
the metadata read and then reads span, version, publication and tombstone in one
statement. Rust issues no further SQL after the evidence statement.

## resolve

`evidence.resolve('docs', '…'::uuid)` →

```json
{"status":"historical","evidence_id":"…","version_id":"…","asset_id":"…","path":"…",
 "start_byte":18,"end_byte":57,"source_sha256":"…","byte_length":57,"published_revision":1,
 "text":"a lost response replays the stored IDs.","verified":true}
```

Purged: same identity fields plus `purged_at` and `reason`, no `text`. Unknown:
`{"status":"not_found"}`. The full source (at most `max_source_bytes`, else
`54000`) is read to recompute the digest and compare the span with its byte
slice; any mismatch is `XX001`. Detection, not protection: a role that rewrites
both source and digest is not detected.

## Verification

Executed so far: host unit tests of the pure modules (request validation, plan
compilation, rendering, DDL) and a type check of the whole crate against
pgrx 0.19.3's bundled PostgreSQL 18 bindings. The in-crate `#[pg_test]` suite
(`src/tests/`) and the external system tests (`tests/system/`) are written for
real PostgreSQL 18 and must be run in the Docker build before any behavior here
is claimed as verified. Gates G1, G4, G6, G7, G8 and G9 remain open until those
results exist; G7 (current-only ANN recall under accumulated history) is not
addressed by v0.1, whose semantic mode post-filters the HNSW index and reports
`underfilled` and `approximate`.
