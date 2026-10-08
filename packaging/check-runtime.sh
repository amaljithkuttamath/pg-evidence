#!/usr/bin/env bash
# Verify the installed package in a fresh runtime image, without a TCP listener.
set -euo pipefail
image="${1:?usage: packaging/check-runtime.sh IMAGE}"
repo="$(cd "$(dirname "$0")/.." && pwd)"
container="pg-evidence-check-$$"
cleanup() { docker rm -f "$container" >/dev/null 2>&1 || true; }
trap cleanup EXIT
docker run --detach --rm --name "$container" --network none \
  --tmpfs /var/lib/postgresql:rw,size=256m \
  -e POSTGRES_HOST_AUTH_METHOD=trust "$image" \
  -c listen_addresses='' -c statement_timeout=10000 >/dev/null
ready=false
for _ in {1..60}; do
  if docker logs "$container" 2>&1 | grep -q 'PostgreSQL init process complete' && \
     docker exec "$container" pg_isready -U postgres >/dev/null 2>&1; then
    ready=true
    break
  fi
  sleep 1
done
if [ "$ready" != true ]; then
  docker logs "$container"
  exit 1
fi
docker exec -i "$container" psql -X -U postgres -v ON_ERROR_STOP=1 \
  < "$repo/packaging/product-smoke.sql"
printf 'PG_EVIDENCE_RUNTIME_OK\n'
