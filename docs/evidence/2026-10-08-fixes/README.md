# Lifecycle and importer regression verification

Both adversarial-review findings were reproduced against the previous ARM package
before changing production code. The new tests failed for six stale-snapshot
interleavings and the near-1MiB importer fixture. See the retained
[concurrency failures](red-concurrency.txt) and [importer failure](red-importer.txt).

The fixed native ARM package passed:

- 62 Rust/backend tests, including standard role-grant checks.
- 30 system-suite tests: 28 database integration tests and two importer chunking
  unit tests. Three lifecycle races each run at four isolation combinations,
  including mixed READ COMMITTED/SERIALIZABLE and both sessions SERIALIZABLE.
- Release installation, package checksums, dump/restore, and a fresh isolated
  Docker runtime check.
- 59 host benchmark tests and protocol validation; 28 packaging tests with two
  historical probe-log tests skipped. Rust formatting and diff checks passed.

The lifecycle tests force the lock wait, verify `40001` for a stale snapshot,
retry with a fresh transaction, and assert no purged current version or retained
embedding. Public content and annotation revision counters remain unchanged by
coordination. The importer test now publishes the 1,048,047-byte file and checks
that concatenating its spans exactly recovers the source bytes.

The implementation uses an actual self-update of the common asset row instead
of a lock alone. This creates a new row version and may add WAL even for an
identical retry; no throughput or write-amplification improvement is claimed.
Chunking prefers a newline only in the last 10% of a chunk, otherwise cutting at
a UTF-8 boundary. Custom small chunks can still require larger response budgets.

## Evidence and reproduction

See [backend tests](step-tests.txt), [system tests](step-system.txt),
[runtime check](step-runtime.txt), [step results](product-results.tsv),
[package checksums](SHA256SUMS), [source hashes](source-SHA256SUMS) and
[verification record](verification.json). All 53 build inputs matched the checkout.
Logs are readable copies; original byte hashes are recorded in verification.json.

```sh
BUILD_JOBS=2 packaging/run-product.sh linux/arm64 "$PWD/packaging/artifacts/product"
docker build --platform linux/arm64 -f packaging/Dockerfile.runtime \
  -t pg-evidence:0.1.0-dev packaging/artifacts/product
packaging/check-runtime.sh pg-evidence:0.1.0-dev
```

This local run reused every toolchain layer, compiled with one Cargo job in
tmpfs, and used a 1 GiB free-space floor checked every second. The default build
guard remains 2.3 GiB. Do not use the reduced floor for a cold toolchain build.

Claude independently reviewed the focused diff; its [static report](claude-review.md)
found no blocking regression. Codex checked the remaining questions: writer/purger
roles inherit SELECT on all tables; lifecycle callers use the same helper; tags
lock the asset; staging is allowed to create a stale staged version whose later
publication must pass revision checks. Backend tests verify grants and SQLSTATEs.
The suggested missing CLI validation was already present: `--max-span-bytes 3`
returns a clean argparse error, not a traceback. The first host-wrapper examples
run had a missing container fixture path; after copying the quickstart fixture,
all six examples/chunk tests passed. The complete native run needed no such wrapper.

No native x86-64, broad performance, ANN recall, or token-saving claim is added.
License selection and public binary/container distribution remain undecided.
