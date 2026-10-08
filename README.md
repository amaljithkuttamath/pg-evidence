# pg-evidence

**Versioned evidence and bounded retrieval for PostgreSQL.**

A Rust extension for agents that search documents, follow explicit relationships,
and cite the exact text they used. Store a source once, retrieve small excerpts,
and resolve a citation after the source changes.

**0.1.0 development preview.** Verified on ARM Linux: 62 Rust/backend tests,
24 system tests and a fresh Docker installation check pass. License selection
and native x86-64 CI activation are pending.
No token-saving, speed or production-scale claim is made.

## What it does

- **Stable citations.** Evidence IDs bind to immutable version IDs and UTF-8 byte
  ranges. Resolving a citation verifies its source digest and exact span.
- **Search in one place.** Literal, regex and PostgreSQL full-text search;
  optional pgvector cosine search with client-generated embeddings.
- **Composable retrieval.** Combine searches, asset tags and one-hop evidence
  relationships before sending results to an agent.
- **Bounded responses.** Limit candidates, excerpts, relationships and the exact
  serialized response size. Truncation and approximate search are explicit.
- **Ordinary PostgreSQL operations.** Invoker permissions, transactions,
  dump/restore and corpus tables that survive dropping the extension.

The use case is an agent that needs inspectable, versioned evidence inside an
existing PostgreSQL deployment. Embedding generation stays in the client. Video
processing, arbitrary code execution and a new storage engine are outside v0.1.

## Build and try it

Requires Docker with BuildKit and Python 3.11+. Rust and PostgreSQL build tools run
inside Docker. Use a host matching the selected architecture, several gigabytes
of free storage and sufficient Docker memory; the development build used 8 GiB.

```sh
git clone https://github.com/amaljithkuttamath/pg-evidence.git
cd pg-evidence

# Use linux/arm64 on Apple silicon; linux/amd64 on an x86-64 host.
BUILD_JOBS=2 packaging/run-product.sh linux/arm64 "$PWD/packaging/artifacts/product"

docker build --platform linux/arm64 -f packaging/Dockerfile.runtime \
  -t pg-evidence:0.1.0-dev packaging/artifacts/product
packaging/check-runtime.sh pg-evidence:0.1.0-dev
```

The build runs Rust/backend tests, installs the release library, runs independent
PostgreSQL system tests, checks dump/restore and compares a retrieval result with
plain SQL. It exports the installable library, control file, SQL, checksums and
logs. Failed attempts retain diagnostics and return a nonzero status.

To use an installed package:

```sql
CREATE EXTENSION pg_evidence;
SELECT evidence.init_collection('docs', '{}'::jsonb);
SET statement_timeout = '10s';

-- After staging and publishing documents:
SELECT evidence.query('docs', '{
  "nodes": [{"id": "hits", "op": "literal", "text": "retrieval", "limit": 5}],
  "output": "hits",
  "excerpt_bytes": 512
}'::jsonb);
```

See [examples](examples/README.md) for ingestion and agent tools,
[the API](docs/api.md) for request/response contracts and
[operations](docs/operations.md) for grants, backup and retention.

## Evidence before claims

[Build evidence](docs/build-environment.md) records the tested platform and
commands. [Benchmarks](docs/benchmark-method.md) separate correctness, latency,
recall, memory and agent token usage. The small SQL smoke comparison checks equal
results; it does not establish an advantage over another extension.

[Autoresearch](docs/autoresearch.md) covers upstream research and bounded code
experiments against a fixed evaluator. Improvements require retained raw results
and review. See the [release gates](docs/roadmap.md) for outstanding work.

## Source map

| Area | Source |
|---|---|
| SQL entry points and PostgreSQL bindings | [src/lib.rs](src/lib.rs), [src/db.rs](src/db.rs) |
| Validation and version lifecycle | [src/model.rs](src/model.rs), [src/ops.rs](src/ops.rs) |
| Query planning and bounded rendering | [src/plan.rs](src/plan.rs), [src/render.rs](src/render.rs) |
| Corpus schema | [src/ddl.rs](src/ddl.rs) |
| Backend and multi-session tests | [src/tests](src/tests), [tests/system](tests/system) |
| SQL comparison arm | [baseline](baseline/README.md) |
| Reproducible builds and install artifacts | [packaging](packaging) |

Contributions should include behavior tests and reproducible verification. Read
[CONTRIBUTING.md](CONTRIBUTING.md) and the [contract](docs/design.md) first.
