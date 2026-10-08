# ARM development verification — 2026-10-08 UTC

The real pg_evidence 0.1.0 extension built, installed and ran on native ARM Linux
inside Docker on an Apple-silicon host. This is developer-preview evidence for
PostgreSQL 18.6, pgvector 0.8.7, Rust 1.99.0 and pgrx 0.19.3.

- 32 pure Rust tests and 30 PostgreSQL backend tests passed.
- 24 independent system tests passed, including concurrent operations, real role
  permissions, citation bytes, restore, baseline comparisons and runnable examples.
- The release library, control file and installation SQL were packaged and their
  checksums verified. A fresh runtime image loaded and exercised the package.
- Host checks: 59 benchmark/protocol tests passed; 28 packaging tests passed and
  two historical probe-log tests skipped because those private logs were absent.
- Five paired SQL smoke samples per arm returned identical result hashes.

See [verification.json](verification.json), [step statuses](product-results.tsv),
[backend tests](step-tests.txt), [system tests](step-system.txt),
[runtime check](step-runtime.txt), [environment](environment.txt) and
[build input hashes](source-SHA256SUMS). Diagnostic logs are readable copies with
ANSI color and trailing whitespace removed, and non-UTF8 bytes escaped. Their original byte hashes are in
the verification record; raw logs remain in the local build output.

## Reproduce

```sh
BUILD_JOBS=2 packaging/run-product.sh linux/arm64 "$PWD/packaging/artifacts/product"
docker build --platform linux/arm64 -f packaging/Dockerfile.runtime \
  -t pg-evidence:0.1.0-dev packaging/artifacts/product
packaging/check-runtime.sh pg-evidence:0.1.0-dev
```

This run reused a cached toolchain and used a 1.5 GiB free-space cancellation
floor with one-second checks. The default cold-build guard remains about 2.3 GiB.
Both Cargo compilation and PostgreSQL test data lived in BuildKit tmpfs. The
runtime image used no network and a disposable database. No cross-host binary
reproducibility or native x86-64 result is claimed.

## Failed attempts retained

1. 55/57 tests passed. Fixed an incorrect at-limit test expectation and explicit
   casts for PostgreSQL internal `char` values in a catalog assertion.
2. 59 tests and release installation passed; 20/21 system tests passed. A graph
   fixture linked two existing search hits but expected a new result. Added a
   non-hit endpoint and kept the expansion assertion.
3. 59 tests, 21 system tests, installation and packaging passed. The host verifier
   rejected a LATIN1 byte in an expected error diagnostic. Made log decoding
   tolerant while preserving raw checksum verification, and added a regression.
4. After review fixes: 62 tests, 24 system tests, install, smoke comparison,
   packaging and fresh-runtime check passed. Current build inputs matched all
   53 entries in the source manifest before publication.

Claude Code performed a separate static review and a second review of the fixes.
Findings led to bounded file reads, short ingestion keys for long paths, safe
example-client string quoting, bounded link-race recovery and an excerpt payload
allocation guard. The reviewer read code and logs; it did not execute tests.

## Limits

The SQL smoke fixture has 1,000 short synthetic spans. Timings include a fresh
`psql` process and connection per sample; the raw observations are retained under
[benchmark-smoke](benchmark-smoke/metadata.json). They are result-equivalence
checks, not evidence of a performance advantage. The wider SQL control has
[documented budget differences](../../../baseline/README.md).

Concurrency tests cover the schedules exercised, including a forced single
link/unlink race. They do not prove every schedule or the branch where unlink wins
twice. The purge test checks retained and purged states around a concurrent commit;
it does not force a resolve to overlap that exact commit instant. The timeout test
checks 57014 propagation, not cancellation after sustained work in every operator.
Native x86-64, history-heavy ANN recall, scale, memory and live-agent token/quality
measurements remain open. No public binary release has been made; license selection
is undecided.
