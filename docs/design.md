> Implementation contract for the 0.1.0 development preview. See
> [build evidence](build-environment.md) for verification and remaining limits.

# Extension contract

A marker such as **G4** names an empirical build gate, listed below. The
contract states required behavior; tests must establish whether its implementation
delivers that behavior. The Rust implementation is in `src/`; release gates
are tracked in [the roadmap](roadmap.md).

### Schemas, encoding and input

- The fixed extension schema is `evidence`. A corpus is a schema created by the
  caller-invoked initializer after `CREATE EXTENSION`, so its tables are not
  extension members. Verify this through catalog tests.
- Corpus names match `^[a-z][a-z0-9_]{0,47}$`. They must not begin with `pg_`, and
  must not be `information_schema`, `public`, `evidence` or the schema holding
  pgvector. `init_collection` creates the schema itself and fails if it already
  exists (`42P06`), so corpus tables never share a schema with unrelated objects.
  The caller becomes the owner.
- No corpus object depends on an object of the `pg_evidence` extension: no
  triggers, defaults, domains, types or check functions from `evidence`. The only
  extension dependency is the pgvector `vector` column, present only when
  embeddings are configured. Assert this through `pg_depend` tests.
- The database encoding must be `UTF8`. Every function checks `server_encoding`
  and refuses otherwise (`55000`). This also catches a corpus restored into a
  non-UTF8 database.
- Source is stored as `text`: the exact UTF-8 bytes received, with no newline,
  Unicode or byte-order-mark normalization. The version digest is SHA-256 over
  those stored bytes. Each stage request carries `source_sha256`; a mismatch
  (for example, after client-encoding conversion) is rejected with `22023`.
- Input that PostgreSQL rejects before the extension runs keeps PostgreSQL's own
  error: invalid UTF-8 text (`22021`), malformed JSON (`22P02`), a JSON `\u0000`
  escape (`22P05`). Binary and non-UTF-8 files are unsupported in v0.1; the
  example importer skips them and reports the path and reason.
- Every collection stores `schema_version = 1` in `collection_config`. Every
  function refuses a corpus whose schema version it does not support (`55000`,
  with a hint naming the required migration). A per-corpus migration function is
  added with the first schema change, not shipped as a placeholder in v0.1. There
  is no extension-owned corpus registry.

Public functions. Request bodies stay `jsonb`; responses are `json`, so
the byte budget applies to the exact text the server returns:

```sql
evidence.init_collection(corpus text, config jsonb) RETURNS void
evidence.stage_version(corpus text, request jsonb) RETURNS json
evidence.attach_embeddings(corpus text, request jsonb) RETURNS void
evidence.publish_version(corpus text, request jsonb) RETURNS json
evidence.retire(corpus text, request jsonb) RETURNS json
evidence.annotate(corpus text, request jsonb) RETURNS json
evidence.purge(corpus text, request jsonb) RETURNS json
evidence.query(corpus text, request jsonb) RETURNS json
evidence.resolve(corpus text, evidence_id uuid) RETURNS json
```

Every JSON request is validated against an explicit discriminated schema with
unknown fields rejected. No embedding provider is called by these functions.

Collection configuration: `embedding_model` and `embedding_dimensions` (both null
for a collection without semantic search; dimensions 1 to 2,000, the pgvector HNSW
limit for `vector`), `text_search_config` (a fixed `regconfig`), and the limits
below. The limits are documented protective defaults, not measured capacity limits.

### Corpus objects

This is the contract that Task 2's canonical DDL must satisfy, not the DDL itself.

| Table | Contents | Writer access |
|---|---|---|
| `collection_config` | One row: schema version, embedding and text-search configuration, limits | Read |
| `assets` | Client-chosen `asset_id`; `current_version_id`, `current_path`, `content_revision`, `annotation_revision`, `retired_at` | Insert; update listed columns only |
| `versions` | Server `version_id`, `asset_id`, path, source text, `source_sha256`, byte length, unique `ingestion_key`, `request_sha256`, `base_revision`, creator, time | Insert only |
| `spans` | Server `evidence_id`; `version_id`; half-open `start_byte < end_byte`; exact span text; stored `tsvector` generated with the fixed configuration. Unique `(version_id, start_byte, end_byte)` | Insert only |
| `embeddings` | `evidence_id`, `vector(N)` | Insert only |
| `publications` | `asset_id`, unique `version_id`, resulting revision, publisher, time | Insert only |
| `tags` | `asset_id`, tag, creator, time; unique `(asset_id, tag)` | Insert, delete |
| `relations` | Source and target `evidence_id` in this corpus, kind, asserting principal, time; unique `(source, kind, target)` | Insert, delete |
| `tombstones` | `version_id`, purger, time, reason | None |

`assets(asset_id, current_version_id)` references `publications(asset_id,
version_id)`, which in turn references `versions(asset_id, version_id)`, so the
current pointer can only name a published version of the same asset. All foreign
keys are ordinary immediate constraints (`NOT DEFERRABLE`); `publish_version`
therefore inserts the publication row before it moves the pointer. A new asset
row starts with `content_revision = 0`, `annotation_revision = 0` and null
`current_version_id`, `current_path` and `retired_at`. Span text
is an immutable copy of `source[start_byte..end_byte]`. Retrieval reads span text and never materializes full sources; only `resolve` reads a
full source, bounded by `max_source_bytes`.

### Roles and trust boundary

Ordinary role and column grants are the authorization boundary. All functions are
`SECURITY INVOKER`. The roles nest: reader within writer within purger.

| Role | Grants |
|---|---|
| Reader | `USAGE` on the corpus schema; `SELECT` on all corpus tables |
| Writer | Reader, plus `INSERT` on `assets`, `versions`, `spans`, `embeddings`, `publications`, `tags`, `relations`; `UPDATE (current_version_id, current_path, content_revision, annotation_revision, retired_at)` on `assets`; `DELETE` on `tags`, `relations` |
| Purger | Writer, plus `UPDATE (source)` on `versions`, `UPDATE (text)` on `spans`, `DELETE` on `embeddings`, `INSERT` on `tombstones` |
| Owner | Trusted; can bypass every invariant |

Consequences, stated so tests can check them:

- A writer holds no `UPDATE` or `DELETE` privilege on retained bytes, digests,
  spans, embeddings or publication records, through the API or direct SQL.
- A writer can still `INSERT` by direct SQL. The API never creates inconsistent
  rows, but a direct insert could add a span to an existing version with text that
  differs from the source slice, or a new version with any content. A table
  `CHECK` constraint ties `source_sha256` to the stored source using the built-in
  `pg_catalog.sha256`, so a version row whose digest does not match its own source
  is rejected at insert. That check does not make inserted evidence true: a writer
  can insert a self-consistent version with false content. Span text cannot be
  checked row-locally: `resolve` detects a span whose text differs from its source
  slice (`XX001`), but `query` excerpts are not re-verified.
- Writer trust therefore covers two things: the correctness of the versions and
  spans a writer newly inserts, and visibility. By direct SQL a writer can move
  the current pointer to any published version of the asset, or alter tags and
  relations, bypassing the API's revision checks. What no writer can do is modify
  or delete bytes already retained: an existing citation's version source, span
  text, digest, embeddings and publication record are outside every writer grant.
- The purger, owner and superusers can alter retained bytes. `resolve` recomputes
  the version digest and compares span text to the source slice; a mismatch
  raises `XX001`. This detects corruption. It does not protect against a role
  that can rewrite both content and digest.
- `SELECT ... FOR UPDATE` requires `UPDATE` privilege on at least one column; the
  writer's `assets` column grant satisfies it (**G10**).
- The implementation must document grants as SQL and exercise them in tests. v0.1 has
  no grant-helper function.

Rejected: a `SECURITY DEFINER` write path (conflicts with the invoker constraint
and adds a privileged surface); immutability triggers (owners can disable them,
and they would make corpus tables depend on the extension, so `DROP EXTENSION ...
CASCADE` would silently remove them).

### Identity, tags and relations

- `asset_id` is chosen by the client (a UUID), including for a new asset. This
  lets an idempotent retry of a new-asset stage converge on one asset row.
- `version_id` and `evidence_id` are generated by the server and returned by
  `stage_version` in request span order. An evidence ID is unique within the
  corpus and permanently bound to one `(version_id, start_byte, end_byte)`. It is
  never reused or reassigned, and survives purge.
- **Tags scope assets.** They form the asset's current tag set and therefore
  carry across versions. `tag` and `untag` lock the asset row, compare
  `expected_annotation_revision` (`55000` on mismatch) and increment
  `annotation_revision`, which is independent of `content_revision`. Tagging
  therefore never causes a publication revision conflict, and publication never
  causes a tagging conflict. Both update the same `assets` row, so one call can
  wait on the other's row lock until it commits. Tag history is not retained in
  v0.1.
- **Relations bind evidence IDs.** Endpoints are immutable, version-specific
  evidence, so a relation is always tied to the exact bytes it was asserted
  about; that binding is how v0.1 meets the proposal's revision precondition for
  relations. Relations are never carried forward to a new version. `link` and
  `unlink` take no revision and no explicit asset lock. PostgreSQL still takes its
  implicit locks (foreign-key checks on the endpoint spans, unique-index waits on
  a concurrent identical insert), so a link can wait briefly. A repeated `link` returns the
  existing relation. Self-links are rejected; cycles are permitted because
  traversal is one hop.
- `neighbors` returns every endpoint with `status` of `current`, `historical`,
  `staged`, `retired` or `purged`. A plan can filter on status. Purged endpoints
  carry no excerpt.

### Mutations, retries and concurrency

Mutations are supported at `READ COMMITTED`. At `REPEATABLE READ` or
`SERIALIZABLE`, a race can raise a genuine PostgreSQL `40001`, which is safe to
retry; the exact behavior is **G6**. Each function call is atomic: an error leaves
no writes from that call.

- **`stage_version`** (`asset_id`, `path`, `source`, `source_sha256`, `spans`
  as `start_byte`/`end_byte` pairs, `ingestion_key`, `expected_revision`):
  1. Validate, then compute `request_sha256` over the canonical request.
  2. Look up `ingestion_key` first. Same request digest: return the stored IDs
     without any other check, so a retry after a lost response succeeds even if
     the asset has since changed. Different digest: `23505`.
  3. `expected_revision` must be a non-negative integer (`22023`). Insert the
     asset if absent (`ON CONFLICT DO NOTHING`) with both revisions 0. A new
     asset is staged with `expected_revision = 0`. If this call inserted the
     asset row and `expected_revision` is not 0, fail with `55000`
     (`revision_conflict`, current revision 0); the call writes nothing, including
     the asset row. If the asset already existed, the base is recorded without
     comparison and `publish_version` compares it.
  4. Insert the version with `base_revision = expected_revision` using
     `ON CONFLICT (ingestion_key) DO NOTHING`. If no row was inserted because a
     concurrent identical request committed first, reselect and apply step 2.
  5. Staging takes no explicit asset lock and does not change current retrieval.
     Because step 2 precedes step 3, a retry after a lost response returns the
     stored result even when its base would now be rejected.
- **`attach_embeddings`** (`version_id`, `model`, vectors keyed by evidence ID):
  locks the asset row `FOR UPDATE` first and holds it for every check and write
  below, so attachment is serialized with purge and publish. All or nothing per
  call. In order:
  1. The version is purged: `55000`. This is checked before any retry is
     accepted, because purge deleted the embeddings a retry would match.
  2. Wrong model or dimension, non-finite values, zero norm, or an evidence ID
     that is not a span of this version: `22023`.
  3. A requested vector differs from one already stored for that evidence ID
     (compared after conversion to the stored `float4` values): `23505`.
  4. Every requested vector is already stored identically: success with no
     write, whether the version is staged or published. This covers a retry
     after a lost acknowledgement, including one sent after publication.
  5. The version is published and the request would insert any vector: `55000`.
     Published embeddings are never added to or changed.
  6. Otherwise insert the missing vectors on the staged version.
- **`publish_version`** (`version_id`): lock the asset row `FOR UPDATE`, then:
  - The version already has a publication record: return that record, even if
    later versions superseded it. Publication is idempotent per version.
  - The version is purged: `55000`.
  - `base_revision` differs from the asset's `content_revision`: `55000`, with
    the current revision in the error detail.
  - Embeddings are configured and any span lacks one: `55000`.
  - Otherwise, still under the lock: compute the new revision as
    `content_revision + 1`; insert the publication record carrying it; then
    update the asset's current pointer, path and `content_revision` in one
    `UPDATE`. The publication row exists before the pointer names it, so the
    immediate foreign key is satisfied without deferral. A current-path
    collision with another asset gives `23505` and rolls back the whole call,
    including the publication row.
- **`retire`** (`asset_id`, `expected_revision`): lock, compare (`55000` on
  mismatch), clear the current pointer, set `retired_at`, increment
  `content_revision`. Retired versions stay resolvable. Publishing a version
  staged against the new revision makes the asset current again.
- A rejected publish leaves its staged version staged. Staged versions that are
  never published are retained, excluded from retrieval and reported by
  `resolve` with status `staged`. Only `purge` removes their content. v0.1 has no
  automatic expiry.
- Every function takes at most one explicit asset row lock (`attach_embeddings`,
  `publish_version`, `retire`, `purge`, `tag`, `untag`). Any future multi-asset
  mutation locks rows in ascending `asset_id` order. This prevents
  lock-order deadlocks within single calls only. PostgreSQL's implicit row,
  foreign-key and unique-index locks remain, and a client transaction that
  spans several calls can still deadlock; PostgreSQL then raises a genuine
  `40P01`, which the client retries by rerunning the whole transaction.

| SQLSTATE | Meaning |
|---|---|
| `22023` | Request failed extension validation |
| `22021`, `22P02`, `22P05` | Rejected by PostgreSQL before the function runs |
| `23505` | Ingestion key reused with different content; conflicting embeddings; current-path collision |
| `42501` | Missing privilege (raised by PostgreSQL) |
| `42P06` | Corpus schema already exists |
| `54000` | Configured size limit exceeded, or the response envelope cannot fit |
| `55000` | Precondition failed: stale revision (including a nonzero base for a new asset), purged or current version, new embeddings on a published version, missing embeddings, unsupported schema version, non-UTF8 database, `statement_timeout` unset |
| `57014` | Cancellation or timeout; always propagated, never converted to success |
| `XX001` | Retained bytes fail digest or span verification |
| `40001`, `40P01` | Only genuine PostgreSQL serialization failures and deadlocks |

Stale revisions deliberately avoid `40001`: PostgreSQL recommends retrying that
code unconditionally, and a stale revision needs a re-read, not a blind retry.
Each `55000` carries a machine-readable reason (for example
`revision_conflict`) in the error detail if pgrx supports it, otherwise as a
message prefix (**G5**).

### Purge

`purge` (`version_id`, `reason`) requires the purger role. It locks the asset,
refuses the current version (`55000`), and is idempotent for an already purged
version. In one transaction it sets the version's source and its spans' text to
null (the stored `tsvector` follows), deletes the spans' embeddings and inserts a
tombstone. It retains IDs, offsets, path, digest, byte length, publication
records and relations. `resolve` then returns `{"status": "purged", ...}` with no
content and never substitutes another version.

Purge is logical removal. Purged bytes persist in dead tuples until `VACUUM`, and
in WAL, replicas, backups and earlier dumps. The retained digest can confirm a
guess of short, low-entropy content. The operations guide must state these limits.

### Queries, budgets and snapshots

- Budgets bound returned data and plan size, not rows scanned, CPU or memory:
  - `max_source_bytes`: 1 MiB per staged source.
  - `max_response_bytes`: 64 KiB, measured on the returned `json` text.
  - `max_candidates_per_operator`: 256 returned rows per operator.
  - `max_plan_nodes`: 32.
  - `max_edges_returned`: 1,024 per plan.
- Database work is bounded only by `statement_timeout`. `query` refuses to run
  when `statement_timeout` is 0 (`55000`). Matched SQL baselines run under the
  same timeout. Every Rust loop over candidates, edges or output calls
  `check_for_interrupts!`.
- Regex mode stays in scope. It uses PostgreSQL's regular-expression dialect via
  the `~` operator. Patterns are limited to 1 KiB (`22023`). It has no index or
  latency guarantee beyond the timeout.
- **One statement for evidence retrieval.** Each call first reads trusted
  collection configuration and extension metadata. `query` then compiles all
  retrieval, filtering, traversal and excerpt fetching into one SQL statement.
  Rust renders those results without fetching evidence in additional statements.
  Evidence therefore comes from one statement snapshot (**G4**); the implementation
  does not claim one SPI call including metadata. `resolve` likewise
  reads tombstone, version and span in one statement, so it cannot interleave
  with a concurrent purge. An unknown evidence ID returns
  `{"status": "not_found"}`; staged and purged IDs return their status.
- Results carry `evidence_id`, `version_id`, `asset_id`, path, offsets, excerpt,
  mode, endpoint status, and a truncation record: `requested`, `returned`,
  `truncated` and `underfilled`. Semantic results also carry
  `approximate: true`. The envelope reports `readiness` per mode (`ready`,
  `not_configured`, or `unavailable` when configured pgvector is missing), meeting the proposal's readiness requirement at collection
  level. No truncated or approximate result is labelled complete.
- If query rendering exceeds `max_response_bytes`, results are dropped whole
  from the end and `truncated` is set. JSON bytes are never cut. If even the
  envelope cannot fit, the call fails with `54000`. Mutation and resolve
  responses must fit in full or fail with `54000`; a mutation error rolls back
  its writes. This preserves all returned citation IDs and exact resolved text.
- A collection with embedding configuration requires embeddings for every span
  before publication. All current evidence is therefore semantically ready.
  A collection without one rejects semantic mode (`22023`).
- The design that keeps historical embeddings from reducing current-only ANN
  recall is **G7**: pgvector iterative scans over all embeddings, or a separately
  indexed set of current embeddings maintained at publish. Neither is assumed.
- Dynamic SQL quotes corpus identifiers, qualifies `pg_catalog` functions and
  operators, qualifies pgvector's type and operators with its schema read from
  `pg_extension` at call time, and uses the collection's fixed text-search
  configuration. Every function also sets `search_path = pg_catalog, pg_temp`
  (**G3**).
- Optional token limits are enforced by a named client tokenizer and labelled
  client-side; byte limits are always enforced natively.

### Function attributes

| Function | Volatility | Parallel | Security | Null input |
|---|---|---|---|---|
| `query`, `resolve` | `STABLE` | `UNSAFE` | Invoker | `STRICT` |
| All other functions | `VOLATILE` | `UNSAFE` | Invoker | `STRICT` |

Parallel safety stays `UNSAFE` until a test justifies otherwise. `STABLE` alone is
not accepted as proof of snapshot behavior on the Rust SPI path; **G4** proves it.

### Build gates

These are empirical questions with named experiments. G2 now has a development
result: on ARM Linux with pgrx 0.19.3, both `src/` database tests passed; the
separate `tests/` test was discovered but failed because its SQL function was
absent from the installed extension. Use `src/tests/` modules included from the
extension crate. This does not establish native x86-64 support (G1). Other gates
remain unproven. See `build-environment.md` for development evidence.

| Gate | Question | Experiment | Decide by |
|---|---|---|---|
| G1 | Which Rust, pgrx, cargo-pgrx, pgvector and PG18 versions work together on Linux x86-64? | Build and load a sample extension in a disposable environment | Task 1 |
| G2 | Where can pgrx database tests live? | Run one `#[pg_test]` from `src/` and attempt one from `tests/` | Task 1 |
| G3 | Does the pinned pgrx emit the declared volatility, parallel, strictness, security and `search_path` settings? | Inspect generated SQL and `pg_proc` | Task 3 |
| G4 | Does one `query` or `resolve` call observe one snapshot after a write earlier in the transaction and during a concurrent publish or purge? | Two-session test at both isolation levels | Tasks 5 and 7 |
| G5 | Can the pinned pgrx raise the chosen SQLSTATEs with a detail field? | Raise each code and read it from a client | Task 3 |
| G6 | Do concurrent identical stages converge, and what happens at `REPEATABLE READ`? | Two-session retry and lost-response tests | Task 4 |
| G7 | Which current-only ANN design holds recall under 1, 5 and 20 retained versions and filter selectivity? A separate current-embedding table would also change the grant matrix, publish and purge. | Recall sweep against exact search | Task 5, before the Task 9 freeze |
| G8 | Do `DROP EXTENSION` (with and without `CASCADE`) and dump/restore behave as stated? | Catalog and restore tests | Task 4 and Task 8 |
| G9 | Does cancellation or timeout stop backend work for regex, lexical and graph queries? | Observe `pg_stat_activity` after cancel | Task 7 |
| G10 | Do the column grants allow the API path and deny direct byte edits, including `FOR UPDATE`? Does the digest `CHECK` reject a forged version insert at acceptable cost for 1 MiB sources? | Role tests with direct SQL; insert timing | Task 2 |
