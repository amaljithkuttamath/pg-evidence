# Try pg-evidence

These examples use the nine SQL functions in [the API](../docs/api.md). They are
included in the real PostgreSQL system suite. No model API or embedding provider
is required for these examples.

## SQL quickstart in Docker

First build `pg-evidence:0.1.0-dev` using the repository README. Start a disposable
local database with no network access; all commands use its local socket:

```sh
docker run -d --rm --name pg-evidence-demo --network none \
  --tmpfs /var/lib/postgresql:rw,size=256m \
  -e POSTGRES_HOST_AUTH_METHOD=trust pg-evidence:0.1.0-dev \
  -c listen_addresses='' -c statement_timeout=10000

# Wait for "PostgreSQL init process complete" and the final server to be ready.
docker logs pg-evidence-demo
docker exec pg-evidence-demo pg_isready -U postgres

docker exec -i pg-evidence-demo psql -X -U postgres -v ON_ERROR_STOP=1 \
  < examples/quickstart.sql

# Removes the disposable database and its data.
docker stop pg-evidence-demo
```

The script stages and publishes a document, retains one citation, publishes an
edit, then resolves the original citation. Its result is `historical` with the
original bytes. The script creates the `quickstart` corpus and expects a fresh
database.

## Import text and expose agent tools

For an existing PostgreSQL server with the extension installed, use Python 3.11+
and `psql` on PATH. Standard PostgreSQL connection variables apply; credentials
can stay in your normal PostgreSQL password file.

```sh
export PGHOST=localhost PGPORT=5432 PGDATABASE=example PGUSER=ingest_service
python3 examples/import_files.py --corpus docs --root ./documents --init
python3 examples/agent_tool.py schema
python3 examples/agent_tool.py search docs 'retry semantics' --hops
python3 examples/agent_tool.py cite docs EVIDENCE_UUID
```

Create the corpus as an authorized owner or omit `--init` for an existing corpus.
Grant the ingest and retrieval roles using [operations](../docs/operations.md).
The importer reports skipped binary/non-UTF-8 files, skips unchanged content,
and uses deterministic asset IDs and ingestion keys. It does not compute vectors.

`agent_tool.run_tool(name, args)` is the integration point for an agent framework.
The adapter returns structured results and database errors. Search composes
lexical hits and optional current one-hop neighbors; cite verifies exact retained
text. This is a minimal example, not an MCP server or a production connection pool.
Server byte budgets cover the database response; framework serialization and tool
message overhead must be accounted for by the caller.
