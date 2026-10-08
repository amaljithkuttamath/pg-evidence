# Development build

The probe has built and loaded on ARM Linux with PostgreSQL 18.6, pgvector 0.8.7,
Rust 1.99.0 and pgrx 0.19.3. A second temporary build with the committed Cargo.lock
produced matching library, SQL and control-file checksums. Both used the same
cached environment layers; cross-host bit-for-bit reproducibility is not claimed.
Exact observed versions and checksums are in [versions.json](../packaging/versions.json).

GitHub CI runs the same probe on native x86-64 Linux. Check the actual workflow
result before claiming that target is verified. This proves the development
stack, not product behavior or benchmark performance.

## Run

```sh
BUILD_JOBS=2 packaging/run-probe.sh linux/amd64 "$PWD/packaging/artifacts/probe/out"
python3 packaging/check_probe.py packaging/artifacts/probe/out
```

Use `linux/arm64` on an ARM host. Docker/BuildKit and Python 3.11+ are required.
Allow several gigabytes of free storage and sufficient Docker VM memory. The
runner refuses to start or cancels when free space on `GUARD_PATH` falls below
`MIN_FREE_KB` (approximately 2.3 GiB by default). Lower Cargo parallelism reduces
memory pressure. This guard is a cancellation threshold, not a maximum-disk guarantee.

The BuildKit probe exports only logs. Cargo build outputs and PostgreSQL data use
temporary memory-backed storage. It installs as an unprivileged user and starts
PostgreSQL with a local socket and no TCP listener. No host PostgreSQL is required.
The wrapper checks actual step outcomes after export; successful export alone is
not a successful probe.

## Test layout

Two `#[pg_test]` functions compiled into the extension crate passed in PostgreSQL.
The separate integration crate under `packaging/probe/tests/` was discovered, but
its database function was absent from the installed extension. That expected
negative result is preserved as a layout experiment, not reported as a pass.

Product backend tests will be modules included in the extension crate, using
`src/tests/`. That split-module arrangement will be verified during scaffolding.
The physical directory name alone does not determine whether a module is compiled
into the extension.

## Reproducibility limits

The base image is digest-pinned. Rust, cargo-pgrx, pgrx and PostgreSQL development
headers have explicit versions; Cargo.lock pins the probe's Rust dependencies.
Other Debian package versions are recorded but not all pinned. The PostgreSQL
package may move to its archive when superseded. A release needs a clean build
and installation check on its declared target, with its source and binary hashes.

This repository has no product release or performance measurements yet.
