#!/usr/bin/env bash
# BuildKit keeps diagnostics on failure; run-product.sh validates the final result.
set -uo pipefail
export CARGO_HOME=/work/cargo-home CARGO_TARGET_DIR=/work/target
export CARGO_PROFILE_DEV_DEBUG=0 CARGO_BUILD_JOBS="${CARGO_BUILD_JOBS:-2}"
export PATH="$(pg_config --bindir):$PATH"
mkdir -p /out
: > /out/product-results.tsv
cp -r /src/product /work/product
cd /work/product || exit 1
step() {
  local name="$1"; shift
  "$@" > "/out/step-$name.log" 2>&1
  local rc=$?
  printf '%s\t%s\n' "$name" "$rc" >> /out/product-results.tsv
  tail -40 "/out/step-$name.log"
  return "$rc"
}
# First generation is exported and committed before the final locked build.
if [ ! -f Cargo.lock ]; then
  cargo generate-lockfile > /out/lockfile.log 2>&1 || exit 1
fi
cp Cargo.lock /out/Cargo.lock.used
find . -type f ! -path '*/__pycache__/*' -print0 | sort -z | xargs -0 sha256sum > /out/source-SHA256SUMS
rustc -Vv > /out/environment.txt
cargo pgrx --version >> /out/environment.txt
pg_config --version >> /out/environment.txt
uname -m >> /out/environment.txt
dpkg-query -W >> /out/environment.txt
step tests cargo pgrx test pg18 --cargo '--locked' || exit 1
rm -rf "$CARGO_TARGET_DIR/debug" "$CARGO_TARGET_DIR/test-pgdata"
step install cargo pgrx install --release --pg-config "$(command -v pg_config)" --cargo '--locked' || exit 1
step system bash packaging/product-system.sh || exit 1
package() {
  mkdir -p /out/package/lib /out/package/extension
  cp "$(pg_config --pkglibdir)/pg_evidence.so" /out/package/lib/
  cp "$(pg_config --sharedir)"/extension/pg_evidence.control /out/package/extension/
  cp "$(pg_config --sharedir)"/extension/pg_evidence--*.sql /out/package/extension/
  cd /out || return 1
  find package -type f -print0 | sort -z | xargs -0 sha256sum > SHA256SUMS
  tar -czf "pg-evidence-0.1.0-pg18-$(uname -m)-linux.tar.gz" package SHA256SUMS source-SHA256SUMS environment.txt
}
step package package || exit 1
printf 'complete\t0\n' >> /out/product-results.tsv
