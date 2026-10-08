#!/usr/bin/env bash
# Build the environment and run the pgrx probe (G1/G2) inside BuildKit, export
# its logs, then validate them with packaging/check_probe.py. No image is
# exported, which avoids storing a second unpacked copy of the toolchain layers.
#
# Usage: packaging/run-probe.sh PLATFORM OUT_DIR
#   PLATFORM  linux/arm64 (development) or linux/amd64 (release target)
# Environment:
#   BUILD_JOBS       cargo parallelism (default 4)
#   MIN_FREE_KB      refuse to start, or cancel the build, if free space on
#                    GUARD_PATH is below this (default 2411724 KiB, about 2.3 GiB)
#   GUARD_PATH       filesystem holding Docker's disk image (default $HOME)
#   GUARD_INTERVAL   seconds between free-space checks (default 5)
# Exit: 0 probe evidence accepted; 1 evidence rejected (logs kept in OUT_DIR);
#       3 low free space (before or during the build); otherwise the build's
#       own exit status. The probe stage always re-runs (--no-cache-filter probe).
set -euo pipefail

platform="$1"; out="$2"
repo="$(cd "$(dirname "$0")/.." && pwd)"
min_free="${MIN_FREE_KB:-2411724}"
guard_path="${GUARD_PATH:-$HOME}"
interval="${GUARD_INTERVAL:-5}"

free_kb() { df -k "$guard_path" | awk 'NR==2 {print $4}'; }

low_space() {
    local free
    free="$(free_kb)"
    if [ "$min_free" -gt 0 ] && [ "$free" -lt "$min_free" ]; then
        echo "run-probe: free space ${free} KiB below ${min_free} KiB; $1" >&2
        return 0
    fi
    return 1
}

if low_space "not starting the build"; then
    exit 3
fi
mkdir -p "$out"

docker buildx build --progress=plain --platform "$platform" \
    -f "$repo/packaging/Dockerfile.build" \
    --build-arg BUILD_JOBS="${BUILD_JOBS:-4}" \
    --target probe-results --no-cache-filter probe \
    --output "type=local,dest=$out" "$repo" &
build=$!

while kill -0 "$build" 2>/dev/null; do
    if low_space "cancelling build"; then
        # Background jobs of a non-interactive shell ignore SIGINT; use TERM.
        kill -TERM "$build" 2>/dev/null || true
        wait "$build" || true
        exit 3
    fi
    sleep "$interval"
done
status=0
wait "$build" || status=$?
if [ "$status" -ne 0 ]; then
    exit "$status"
fi

# Export success is not probe success: the probe RUN records failures and
# succeeds so its logs can be exported.
python3 -I "$repo/packaging/check_probe.py" "$out"
