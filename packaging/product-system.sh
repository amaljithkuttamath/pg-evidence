#!/usr/bin/env bash
set -euo pipefail
export PATH="$(pg_config --bindir):$PATH"
export PGHOST=/work PGUSER=builder PGDATABASE=postgres
initdb -D /work/system-pgdata -E UTF8 --locale=C.UTF-8 -U builder >/dev/null
pg_ctl -D /work/system-pgdata -o "-k /work -c listen_addresses='' -c statement_timeout=10000" -w start
trap 'pg_ctl -D /work/system-pgdata -m fast -w stop' EXIT
psql -X -v ON_ERROR_STOP=1 -f packaging/product-smoke.sql
psql -X -At -v ON_ERROR_STOP=1 -c 'SELECT row_to_json(s)::text FROM smoke_corpus.spans s ORDER BY evidence_id' > /work/spans-before.txt
pg_dump -Fc -f /work/product.dump
createdb evidence_restore
pg_restore --exit-on-error --dbname=evidence_restore /work/product.dump
psql -X -At -v ON_ERROR_STOP=1 -d evidence_restore -c 'SELECT row_to_json(s)::text FROM smoke_corpus.spans s ORDER BY evidence_id' > /work/spans-after.txt
cmp /work/spans-before.txt /work/spans-after.txt
psql -X -v ON_ERROR_STOP=1 -d evidence_restore <<'SQL'
DO $$ BEGIN
  IF NOT has_table_privilege('smoke_reader','smoke_corpus.spans','SELECT') THEN
    RAISE EXCEPTION 'restore lost reader grants';
  END IF;
  IF (SELECT count(*) FROM smoke_corpus.versions) <> 2 THEN
    RAISE EXCEPTION 'restore lost versions';
  END IF;
  IF (SELECT count(*) FROM smoke_corpus.spans) <> 2 THEN
    RAISE EXCEPTION 'restore lost citation IDs';
  END IF;
END $$;
DROP EXTENSION pg_evidence;
DO $$ BEGIN
  IF (SELECT count(*) FROM smoke_corpus.versions) <> 2 THEN
    RAISE EXCEPTION 'extension drop lost corpus';
  END IF;
END $$;
CREATE EXTENSION pg_evidence;
SELECT evidence.resolve('smoke_corpus', evidence_id) FROM smoke_corpus.spans;
SQL
# Additional independent multi-session/role tests, when supplied by the product.
if compgen -G 'tests/system/test_*.py' > /dev/null; then
  python3 -m unittest discover -s tests/system -p 'test_*.py'
fi
psql -X -v ON_ERROR_STOP=1 -f bench/sql/smoke_fixture.sql
python3 -m bench.compare_sql --baseline bench/sql/smoke_sql.sql \
  --candidate bench/sql/smoke_extension.sql --repetitions 5 \
  --output /out/benchmark-smoke
printf 'PG_EVIDENCE_SYSTEM_OK\n'
