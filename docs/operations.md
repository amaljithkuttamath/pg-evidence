# Operating pg-evidence v0.1

Development software: verify every statement here against the Docker test
results before relying on it. See [API](api.md) and [contract](design.md).

## Install

PostgreSQL 18, a `UTF8` database, and pgvector only if a collection uses
embeddings. The extension is untrusted native code: a superuser runs

```sql
CREATE EXTENSION pg_evidence;      -- creates schema "evidence" with 9 functions
CREATE EXTENSION vector;           -- optional, before embedding collections
```

Function `EXECUTE` is granted to `PUBLIC` by default, but schema `evidence` is
not usable until granted (below). The functions are `SECURITY INVOKER`: they can
only do what the caller's own grants on corpus tables allow.

## Roles and grants

Roles nest: reader within writer within purger; the corpus owner is trusted and
can bypass every invariant. Create group roles once (`sql/roles.sql`), then grant
per corpus as its owner (`sql/grants.sql`):

```sh
psql -v reader=evidence_reader -v writer=evidence_writer -v purger=evidence_purger -f sql/roles.sql
psql -c "SELECT evidence.init_collection('docs', '{}')"
psql -v corpus=docs -v reader=evidence_reader -v writer=evidence_writer \
     -v purger=evidence_purger -f sql/grants.sql
psql -c "GRANT evidence_writer TO ingest_service"   # login roles join a group
```

| Role | Grants |
|---|---|
| reader | `USAGE` on `evidence` and the corpus schema; `SELECT` on all corpus tables |
| writer | reader + `INSERT` on assets, versions, spans, embeddings, publications, tags, relations; `UPDATE (current_version_id, current_path, content_revision, annotation_revision, retired_at)` on assets; `DELETE` on tags, relations |
| purger | writer + `UPDATE (source)` on versions, `UPDATE (text)` on spans, `DELETE` on embeddings, `INSERT` on tombstones |

A writer has no `UPDATE`/`DELETE` on retained bytes, digests, spans, embeddings
or publications. Writers can still insert rows by direct SQL: a `CHECK`
constraint rejects a version whose digest does not match its own source, but a
self-consistent false version or a span whose text differs from its source slice
can be inserted (the latter is detected by `resolve`, `XX001`; `query` excerpts
are not re-verified). Writers can also move the current pointer or edit tags and
relations directly, bypassing revision checks. The digest check is evaluated on
every version insert; its cost for 1 MiB sources is not yet measured (G10).

## Timeouts, cancellation and limits

`evidence.query` refuses to run with `statement_timeout = 0`. Set it per role or
session (`ALTER ROLE agent SET statement_timeout = '5s'`); it is the only bound
on database CPU and memory for regex, lexical, vector and graph work. Budgets
(see API "Limits") bound returned data and plan size, not rows scanned.
Cancellation and timeouts are PostgreSQL's `57014` and are never converted to
success. Rust loops over candidates, edges and output check for interrupts;
request validation runs over already parsed, bounded input.

## Embeddings

Vectors are computed outside the database and attached before publication;
publication requires a vector for every span. Semantic queries use the HNSW
index (cosine) on all retained embeddings and keep only current evidence, so
history can reduce how many current results the index returns: results then
report `underfilled` and are always `approximate`. Measuring and fixing this is
gate G7; tune `hnsw.ef_search` per session if needed. Purge deletes a version's
embeddings.

## Purge is logical removal

`purge` nulls the source and span text and deletes embeddings in one
transaction, keeping IDs, offsets, digests, byte lengths, publications,
relations and a tombstone. The bytes still exist in dead tuples until `VACUUM`
(run `VACUUM` on `versions` and `spans` afterwards), in WAL and WAL archives,
physical and logical replicas, backups and earlier dumps, and in any copy a
client made. The retained SHA-256 digest can confirm a guess of short or
low-entropy content. Treat purge as hiding content from this database, not as
erasure; erasure requires handling every copy above.

## Backup, restore and removal

- Corpus schemas are ordinary schemas owned by their creator, not extension
  members; `pg_dump`/`pg_restore` carry their rows, IDs, digests and grants. The
  target must have pg_evidence (and pgvector, if used) installed and the roles
  created. `tests/system/test_restore_system.py` compares bytes, IDs and ACLs.
- `DROP EXTENSION pg_evidence` (with or without `CASCADE`) removes only the
  functions; corpus data stays. `CREATE EXTENSION pg_evidence` resumes service
  (re-grant `USAGE` on `evidence` if the schema was dropped).
- Dropping pgvector with `CASCADE` drops the `embedding` columns. Do not.
- Each corpus records `schema_version = 1`; a future release that changes the
  layout will ship a migration and refuse older layouts with `55000`.

## Monitoring

Useful checks: rows in `versions` without publication (staged versions are
kept forever until purged); `tombstones` growth; `pg_stat_user_tables` dead
tuples after purge; HNSW index size; `pg_stat_activity` for long `evidence.query`
calls. No metrics are exported by the extension.

## Open gates

The native ARM build, current concurrency schedules, role checks and restore suite
have passed; see [the verification record](evidence/2026-10-08-arm64/README.md).
Native x86-64 (G1), history/filter ANN recall (G7), sustained active-work cancellation
(G9) and broader recovery schedules remain open. The 1 ms timeout test establishes
SQLSTATE propagation, not cancellation coverage for every operator. No scale,
process-memory, digest-check cost or token-efficiency advantage has been established.
