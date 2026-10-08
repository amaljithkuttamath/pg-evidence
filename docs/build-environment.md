# Build and verification

The **actual pg_evidence 0.1.0 extension** has built, installed and passed its
current verification suite on native ARM Linux in Docker: 32 pure Rust tests,
30 PostgreSQL backend tests, 24 system tests and a fresh-runtime installation
check. The stack is PostgreSQL 18.6, pgvector 0.8.7, Rust 1.99.0 and pgrx 0.19.3.

The [verification record](evidence/2026-10-08-arm64/README.md) includes commands,
logs, package checksums, exact build-input hashes, failure history and limitations.
Native x86-64 CI remains pending activation; ARM results do not establish x86-64
support. No public binary release, performance advantage or token saving is claimed.

## Build the product

Requires Docker/BuildKit and Python 3.11+. No host Rust or PostgreSQL installation
is needed. Use the platform matching the host:

```sh
BUILD_JOBS=2 packaging/run-product.sh linux/arm64 "$PWD/packaging/artifacts/product"
python3 packaging/packaging_check_product.py packaging/artifacts/product
```

Use `linux/amd64` on an x86-64 host. The toolchain is cached between runs; the
product stage always runs again. Test compilation and PostgreSQL data use tmpfs;
the build exports only logs and install artifacts. Use sufficient Docker memory
(the development VM had about 8 GiB) and several gigabytes of free disk.

The runner refuses to start or cancels when free space on `GUARD_PATH` falls below
`MIN_FREE_KB` (about 2.3 GiB by default). This is a cancellation threshold, not a
maximum-disk guarantee. `BUILD_JOBS` controls toolchain compilation; product
compilation defaults to one job to keep memory pressure down.

A successful output directory contains:

- `package/lib/pg_evidence.so` and `package/extension/` with control and SQL files.
- A compressed install archive, `SHA256SUMS`, environment and source hashes.
- Actual step statuses, backend/system logs and paired SQL smoke observations.

Failed attempts preserve their diagnostics and return a nonzero status. Successful
BuildKit export alone does not mean the tests or package passed.

## Verify the runtime

```sh
docker build --platform linux/arm64 -f packaging/Dockerfile.runtime \
  -t pg-evidence:0.1.0-dev packaging/artifacts/product
packaging/check-runtime.sh pg-evidence:0.1.0-dev
```

The runtime check uses a fresh database, no container network and no TCP listener.
It verifies historical citation resolution and role restrictions, then removes
its disposable container. The image remains available for the [examples](../examples/README.md).

## Reproducibility and remaining gates

The base image is digest-pinned. Rust, cargo-pgrx, pgrx and PostgreSQL headers have
explicit versions; Cargo.lock fixes Rust dependencies. Debian package versions are
recorded, but not all are pinned. Superseded PostgreSQL packages may move to an
archive. Cross-host bit-for-bit reproducibility is not established.

The suite exercises concurrent publication, identical stage retries, tags during
publish, two-node query composition, resolve around purge, and a forced link/unlink
race. At `REPEATABLE READ`, the observed identical-stage race raised PostgreSQL's
genuine `40001`; retry returned the retained IDs. Dump/restore preserved bytes,
IDs and grants, and dropping/recreating the extension preserved corpus data.

Coverage is empirical, not exhaustive. Active cancellation across every operator,
history-heavy/filter-selective ANN recall, controlled scale/memory measurements and
live-agent evaluation remain open. See [release gates](roadmap.md).

## Earlier toolchain probe

`packaging/run-probe.sh` is retained for toolchain investigations. Two backend
functions compiled into its extension passed. A separate integration crate was
intentionally shown to be the wrong layout: its database function was not installed.
The product instead compiles `src/tests/` into its extension crate; that layout
has now run successfully. Earlier probe pins are in [versions.json](../packaging/versions.json).
