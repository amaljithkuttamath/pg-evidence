#!/usr/bin/env bash
# Runs inside the build image as `builder` (see run-probe.sh). Every step's exit
# status is recorded in /out/probe-results.tsv; a failed step is preserved, not hidden.
# This script's own exit status is not the verdict: run-probe.sh validates the
# exported logs with packaging/check_probe.py.
set -uo pipefail

OUT=/out
RESULTS="$OUT/probe-results.tsv"
WORK=/work
PG_BIN="$(pg_config --bindir)"
export CARGO_HOME="$WORK/cargo-home" CARGO_TARGET_DIR="$WORK/target"
export CARGO_BUILD_JOBS="${CARGO_BUILD_JOBS:-4}"
# Debug info dominates target size; dropping it lets the build fit in tmpfs.
export CARGO_PROFILE_DEV_DEBUG=0
: > "$RESULTS"

step() {
    local name="$1"; shift
    local start=$SECONDS
    echo "=== step $name: $*"
    "$@" > "$OUT/step-$name.log" 2>&1
    local rc=$?
    printf '%s\t%s\t%ss\n' "$name" "$rc" "$((SECONDS - start))" | tee -a "$RESULTS"
    tail -n 25 "$OUT/step-$name.log"
    return $rc
}

versions() {
    uname -m
    rustc -Vv
    cargo -V
    cargo pgrx --version
    pg_config --version
    "$PG_BIN/postgres" --version
    grep default_version "$(pg_config --sharedir)/extension/vector.control"
    dpkg-query -W -f '${Package}=${Version}\n' postgresql-18 postgresql-server-dev-18 \
        clang-19 libclang1-19 llvm-19-dev libicu-dev libssl-dev gcc libc6-dev make pkg-config
    grep -E '^(PRETTY_NAME|VERSION_ID)=' /etc/os-release
}

cp -r /src/probe "$WORK/probe"
cp /src/rust-toolchain.toml "$WORK/probe/"
cd "$WORK/probe" || exit 1
LOCKED="--locked"
if [ ! -f Cargo.lock ]; then
    LOCKED=""
    step lockfile cargo generate-lockfile && cp Cargo.lock "$OUT/Cargo.lock"
fi

step versions versions
# G2: one #[pg_test] under src/, one under tests/ (filters select each).
step g2_src cargo pgrx test pg18 src_pg_test ${LOCKED:+--cargo "$LOCKED"}
step g2_tests_dir cargo pgrx test pg18 tests_dir_pg_test ${LOCKED:+--cargo "$LOCKED"}
rm -rf "$CARGO_TARGET_DIR/debug" "$CARGO_TARGET_DIR/test-pgdata"
# G1: release install into the system PostgreSQL, then load beside pgvector.
step install cargo pgrx install --release --pg-config "$(command -v pg_config)" \
    ${LOCKED:+--cargo "$LOCKED"}

load() (
    set -e
    "$PG_BIN/initdb" -D "$WORK/pgdata" -E UTF8 --locale=C.UTF-8 -U builder >/dev/null
    "$PG_BIN/pg_ctl" -D "$WORK/pgdata" -o "-k $WORK -c listen_addresses=''" -w start
    trap '"$PG_BIN/pg_ctl" -D "$WORK/pgdata" -m fast -w stop' EXIT
    "$PG_BIN/psql" -h "$WORK" -d postgres -v ON_ERROR_STOP=1 -AtX <<'SQL'
CREATE EXTENSION vector;
CREATE EXTENSION pg_evidence_probe;
SELECT 'server_version=' || probe_server_version();
SELECT 'cosine_distance=' || probe_vector_cosine_distance();
SELECT 'extension ' || extname || '=' || extversion FROM pg_extension ORDER BY extname;
SELECT 'encoding=' || pg_encoding_to_char(encoding) FROM pg_database WHERE datname = current_database();
SQL
)
step load load

checksums() {
    sha256sum "$(pg_config --pkglibdir)/pg_evidence_probe.so" \
        "$(pg_config --sharedir)"/extension/pg_evidence_probe*
}
step checksums checksums
cp "$WORK/probe/Cargo.lock" "$OUT/Cargo.lock.used" 2>/dev/null
echo "=== results"; cat "$RESULTS"
