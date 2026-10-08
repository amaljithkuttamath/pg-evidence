# pg-evidence

**Versioned evidence and bounded retrieval for PostgreSQL.**

Native x86-64 CI is pending activation. [Development evidence](docs/build-environment.md).

An experimental Rust extension for agents that need to retrieve evidence, follow
explicit relationships and return citations that remain meaningful after a source
changes.

**Development stage:** the Rust/PostgreSQL build probe and benchmark protocol work.
The product extension is not implemented or released yet. License selection is
pending.

## Why build this?

| Problem | Intended benefit | Evidence required |
|---|---|---|
| A source changes after an agent cites it | Resolve the exact retained version and byte span | Update, purge and restore tests |
| Tools send too much text back to the model | Return bounded excerpts and compose operations before rendering | Paired agent trials at a fixed answer-quality margin |
| Search, tags and provenance live in separate services | Query them under PostgreSQL permissions and transactions | Matched SQL baseline, permission and concurrency tests |
| Fast search claims hide recall or memory costs | Report latency, recall, memory and failures together | Filtered/history benchmarks with identical datasets |

These are design goals. **No token-reduction or performance result is claimed.**

## Scope

The proposed first release combines immutable text versions, exact citations,
literal and full-text search, pgvector retrieval, tags, one-hop relationships and
bounded composed queries. Embeddings are generated outside PostgreSQL.

See the [proposed contract](docs/design.md), [roadmap](docs/roadmap.md) and
[benchmark method](docs/benchmark-method.md). Video, a new storage engine and
arbitrary code execution inside the database are outside the initial scope.

## Development quickstart

Requires Python 3.11+ and Docker with BuildKit. Rust and PostgreSQL build tools run
inside Docker. The source tree currently contains a **probe**, not an installable
`pg_evidence` product extension.

```sh
git clone https://github.com/amaljithkuttamath/pg-evidence.git
cd pg-evidence
python3 -m unittest discover -s bench/tests -p 'test_*.py'
python3 -m unittest discover -s packaging/tests -p 'test_*.py'
python3 -m bench.protocol --check bench/protocol.json

# Use linux/amd64 on an x86-64 host; linux/arm64 on Apple silicon.
BUILD_JOBS=2 packaging/run-probe.sh linux/amd64 "$PWD/packaging/artifacts/probe/out"
```

The build runner checks free space, exports diagnostic logs and rejects failed or
missing mandatory probe steps. [Build details and verified scope](docs/build-environment.md).

## Research and releases

[Autoresearch](docs/autoresearch.md) tracks upstream research and bounded benchmark
experiments. Correctness and evaluation rules stay fixed during an experiment.
Every proposed improvement needs reproducible evidence and review.

GitHub will host source, CI evidence and versioned release assets. A usable package
release waits for the core extension and its acceptance tests. No product package
has been published. See [contributing](CONTRIBUTING.md) to help.
