-- Matched SQL baseline: a thin wrapper of prepared statements over a corpus
-- created by baseline/schema.sql (or by evidence.init_collection; the tables
-- are identical). It never calls the pg_evidence extension. Each query is one
-- statement with the extension's ordering, LIMIT k+1 truncation detection,
-- UTF-8-safe excerpt cut and whole-result budget dropping, so both arms answer
-- the same request against the same corpus, indexes and budgets.
--
--   psql -v corpus=docs_sql [-v vector_schema=public -v dims=384] -f baseline/wrapper.sql
--   EXECUTE baseline_literal('needle', 10, 512, 65536);
--
-- Differences from the extension (documented in baseline/README.md): the
-- budget keeps a fixed 256-byte envelope reserve instead of an exact search,
-- and a budget below 256 returns NULL instead of raising 54000.
\set ON_ERROR_STOP on
BEGIN;
SET LOCAL search_path = pg_catalog, pg_temp;

-- Ingest: one statement per version. $1 asset, $2 path, $3 source,
-- $4 ingestion_key, $5 base revision, $6/$7 span starts/ends (UTF-8 bytes).
PREPARE baseline_stage(uuid, text, text, text, bigint, int4[], int4[]) AS
WITH a AS (
  INSERT INTO :"corpus".assets (asset_id) VALUES ($1) ON CONFLICT (asset_id) DO NOTHING),
v AS (
  INSERT INTO :"corpus".versions (version_id, asset_id, path, source, source_sha256, byte_length,
                                  ingestion_key, request_sha256, base_revision)
  VALUES (gen_random_uuid(), $1, $2, $3, sha256(convert_to($3, 'UTF8')), octet_length($3), $4,
          sha256(convert_to($4, 'UTF8')), $5)
  RETURNING version_id),
s AS (
  INSERT INTO :"corpus".spans (evidence_id, version_id, start_byte, end_byte, text)
  SELECT gen_random_uuid(), v.version_id, r.s, r.e,
         convert_from(substring(convert_to($3, 'UTF8') FROM r.s + 1 FOR r.e - r.s), 'UTF8')
  FROM v, ROWS FROM (unnest($6), unnest($7)) WITH ORDINALITY AS r(s, e, ord)
  RETURNING evidence_id, start_byte, end_byte)
SELECT json_build_object('version_id', (SELECT version_id FROM v),
  'spans', (SELECT json_agg(json_build_array(evidence_id, start_byte, end_byte) ORDER BY start_byte, end_byte) FROM s))::text;

-- Publish with the same revision rule: only when base_revision is current.
-- Returns NULL revision when the base is stale.
PREPARE baseline_publish(uuid) AS
WITH v AS (
  SELECT v.version_id, v.asset_id, v.path, v.base_revision
  FROM :"corpus".versions v JOIN :"corpus".assets a ON a.asset_id = v.asset_id
  WHERE v.version_id = $1 AND a.content_revision = v.base_revision
    AND NOT EXISTS (SELECT 1 FROM :"corpus".tombstones t WHERE t.version_id = v.version_id)
  FOR UPDATE OF a),
p AS (
  INSERT INTO :"corpus".publications (asset_id, version_id, revision)
  SELECT asset_id, version_id, base_revision + 1 FROM v RETURNING asset_id, version_id, revision),
u AS (
  UPDATE :"corpus".assets a SET current_version_id = p.version_id, current_path = v.path,
         content_revision = p.revision, retired_at = NULL
  FROM p, v WHERE a.asset_id = p.asset_id RETURNING a.content_revision)
SELECT json_build_object('revision', (SELECT content_revision FROM u))::text;

PREPARE baseline_tag(uuid, text[]) AS
INSERT INTO :"corpus".tags (asset_id, tag) SELECT $1, t FROM unnest($2) AS t ON CONFLICT DO NOTHING;

PREPARE baseline_link(uuid, text, uuid) AS
INSERT INTO :"corpus".relations (source_evidence_id, kind, target_evidence_id) VALUES ($1, $2, $3)
ON CONFLICT DO NOTHING;

-- Queries: $1 query text, $2 limit, $3 excerpt_bytes, $4 max_response_bytes.
PREPARE baseline_literal(text, int, int, int) AS
WITH hits AS MATERIALIZED (
  SELECT q.evidence_id, row_number() OVER (ORDER BY q.path, q.start_byte, q.evidence_id) AS rn
  FROM (SELECT s.evidence_id, v.path, s.start_byte
        FROM :"corpus".spans s
        JOIN :"corpus".assets a ON a.current_version_id = s.version_id
        JOIN :"corpus".versions v ON v.version_id = s.version_id
        WHERE strpos(s.text, $1) > 0
        ORDER BY v.path, s.start_byte, s.evidence_id LIMIT $2 + 1) q),
items AS (
  SELECT h.rn, json_build_object('evidence_id', s.evidence_id, 'version_id', s.version_id,
    'asset_id', v.asset_id, 'path', v.path, 'start_byte', s.start_byte, 'end_byte', s.end_byte,
    'status', 'current', 'mode', 'literal', 'excerpt', ex.excerpt,
    'excerpt_truncated', octet_length(ex.excerpt) < s.end_byte - s.start_byte)::text AS item
  FROM hits h JOIN :"corpus".spans s ON s.evidence_id = h.evidence_id
  JOIN :"corpus".versions v ON v.version_id = s.version_id
  CROSS JOIN LATERAL (SELECT convert_from(substring(raw FROM 1 FOR n - backoff), 'UTF8') AS excerpt
      FROM (SELECT convert_to(left(s.text, $3), 'UTF8') AS raw) bytes
      CROSS JOIN LATERAL (SELECT least($3, octet_length(raw)) AS n) cap
      CROSS JOIN generate_series(0, 3) AS backoff
      WHERE CASE WHEN n - backoff < 0 THEN false
                 WHEN n - backoff = octet_length(raw) THEN true
                 ELSE (get_byte(raw, n - backoff) & 192) <> 128 END
      ORDER BY backoff LIMIT 1) ex
  WHERE h.rn <= $2),
kept AS (
  SELECT rn, item FROM (SELECT rn, item, sum(octet_length(item) + 1) OVER (ORDER BY rn) AS cum FROM items) z
  WHERE cum + 256 <= $4)
SELECT CASE WHEN $4 >= 256 THEN
  '{"results":[' || coalesce((SELECT string_agg(item, ',' ORDER BY rn) FROM kept), '') || '],"truncation":'
  || json_build_object('requested', $2, 'returned', (SELECT count(*) FROM kept),
       'truncated', (SELECT count(*) FROM hits) > $2 OR (SELECT count(*) FROM kept) < (SELECT count(*) FROM items),
       'underfilled', (SELECT count(*) FROM kept) < $2,
       'dropped_for_budget', (SELECT count(*) FROM items) - (SELECT count(*) FROM kept))::text || '}' END;

PREPARE baseline_lexical(text, int, int, int) AS
WITH cfg AS (SELECT text_search_config::regconfig AS c FROM :"corpus".collection_config),
hits AS MATERIALIZED (
  SELECT q.evidence_id, q.score, row_number() OVER (ORDER BY q.score DESC, q.evidence_id) AS rn
  FROM (SELECT s.evidence_id, ts_rank_cd(s.tsv, tq.query)::float8 AS score
        FROM :"corpus".spans s
        JOIN :"corpus".assets a ON a.current_version_id = s.version_id
        CROSS JOIN (SELECT websearch_to_tsquery(cfg.c, $1) AS query FROM cfg) tq
        WHERE s.tsv @@ tq.query
        ORDER BY score DESC, s.evidence_id LIMIT $2 + 1) q),
items AS (
  SELECT h.rn, json_build_object('evidence_id', s.evidence_id, 'version_id', s.version_id,
    'asset_id', v.asset_id, 'path', v.path, 'start_byte', s.start_byte, 'end_byte', s.end_byte,
    'status', 'current', 'mode', 'lexical', 'excerpt', ex.excerpt,
    'excerpt_truncated', octet_length(ex.excerpt) < s.end_byte - s.start_byte, 'rank', h.score)::text AS item
  FROM hits h JOIN :"corpus".spans s ON s.evidence_id = h.evidence_id
  JOIN :"corpus".versions v ON v.version_id = s.version_id
  CROSS JOIN LATERAL (SELECT convert_from(substring(raw FROM 1 FOR n - backoff), 'UTF8') AS excerpt
      FROM (SELECT convert_to(left(s.text, $3), 'UTF8') AS raw) bytes
      CROSS JOIN LATERAL (SELECT least($3, octet_length(raw)) AS n) cap
      CROSS JOIN generate_series(0, 3) AS backoff
      WHERE CASE WHEN n - backoff < 0 THEN false
                 WHEN n - backoff = octet_length(raw) THEN true
                 ELSE (get_byte(raw, n - backoff) & 192) <> 128 END
      ORDER BY backoff LIMIT 1) ex
  WHERE h.rn <= $2),
kept AS (
  SELECT rn, item FROM (SELECT rn, item, sum(octet_length(item) + 1) OVER (ORDER BY rn) AS cum FROM items) z
  WHERE cum + 256 <= $4)
SELECT CASE WHEN $4 >= 256 THEN
  '{"results":[' || coalesce((SELECT string_agg(item, ',' ORDER BY rn) FROM kept), '') || '],"truncation":'
  || json_build_object('requested', $2, 'returned', (SELECT count(*) FROM kept),
       'truncated', (SELECT count(*) FROM hits) > $2 OR (SELECT count(*) FROM kept) < (SELECT count(*) FROM items),
       'underfilled', (SELECT count(*) FROM kept) < $2,
       'dropped_for_budget', (SELECT count(*) FROM items) - (SELECT count(*) FROM kept))::text || '}' END;

-- Composed: lexical seeds (limit $2) -> one hop in both directions (endpoint
-- limit $5, edge limit $6) -> union, deduplicated, seeds first.
PREPARE baseline_lexical_neighbors(text, int, int, int, int, int) AS
WITH cfg AS (SELECT text_search_config::regconfig AS c FROM :"corpus".collection_config),
hits AS MATERIALIZED (
  SELECT q.evidence_id, row_number() OVER (ORDER BY q.score DESC, q.evidence_id) AS rn
  FROM (SELECT s.evidence_id, ts_rank_cd(s.tsv, tq.query)::float8 AS score
        FROM :"corpus".spans s
        JOIN :"corpus".assets a ON a.current_version_id = s.version_id
        CROSS JOIN (SELECT websearch_to_tsquery(cfg.c, $1) AS query FROM cfg) tq
        WHERE s.tsv @@ tq.query
        ORDER BY score DESC, s.evidence_id LIMIT $2 + 1) q),
edges AS MATERIALIZED (
  SELECT z.*, row_number() OVER (ORDER BY z.from_rn, z.kind, z.to_id, z.dir) AS edge_rn
  FROM (SELECT x.* FROM (
          SELECT f.rn AS from_rn, r.kind, r.target_evidence_id AS to_id, 0 AS dir
          FROM hits f JOIN :"corpus".relations r ON r.source_evidence_id = f.evidence_id WHERE f.rn <= $2
          UNION ALL
          SELECT f.rn, r.kind, r.source_evidence_id, 1
          FROM hits f JOIN :"corpus".relations r ON r.target_evidence_id = f.evidence_id WHERE f.rn <= $2) x
        ORDER BY x.from_rn, x.kind, x.to_id, x.dir LIMIT $6 + 1) z),
nbr AS MATERIALIZED (
  SELECT q.evidence_id, row_number() OVER (ORDER BY q.first_edge) AS rn
  FROM (SELECT to_id AS evidence_id, min(edge_rn) AS first_edge FROM edges WHERE edge_rn <= $6
        GROUP BY to_id ORDER BY first_edge LIMIT $5 + 1) q),
uni AS MATERIALIZED (
  SELECT q.evidence_id, q.mode, row_number() OVER (ORDER BY q.inp, q.irn) AS rn
  FROM (SELECT DISTINCT ON (x.evidence_id) x.* FROM (
          SELECT evidence_id, 'lexical' AS mode, 0 AS inp, rn AS irn FROM hits WHERE rn <= $2
          UNION ALL
          SELECT evidence_id, 'neighbors', 1, rn FROM nbr WHERE rn <= $5) x
        ORDER BY x.evidence_id, x.inp, x.irn) q
  ORDER BY q.inp, q.irn LIMIT $2 + $5 + 1),
items AS (
  SELECT u.rn, json_build_object('evidence_id', s.evidence_id, 'version_id', s.version_id,
    'asset_id', v.asset_id, 'path', v.path, 'start_byte', s.start_byte, 'end_byte', s.end_byte,
    'status', CASE WHEN t.version_id IS NOT NULL THEN 'purged'
                   WHEN a.current_version_id = v.version_id THEN 'current'
                   WHEN p.version_id IS NULL THEN 'staged'
                   WHEN a.current_version_id IS NULL AND a.retired_at IS NOT NULL THEN 'retired'
                   ELSE 'historical' END,
    'mode', u.mode, 'excerpt', ex.excerpt,
    'excerpt_truncated', coalesce(octet_length(ex.excerpt) < s.end_byte - s.start_byte, false))::text AS item
  FROM uni u JOIN :"corpus".spans s ON s.evidence_id = u.evidence_id
  JOIN :"corpus".versions v ON v.version_id = s.version_id
  LEFT JOIN :"corpus".assets a ON a.asset_id = v.asset_id
  LEFT JOIN :"corpus".publications p ON p.version_id = v.version_id
  LEFT JOIN :"corpus".tombstones t ON t.version_id = v.version_id
  LEFT JOIN LATERAL (SELECT convert_from(substring(raw FROM 1 FOR n - backoff), 'UTF8') AS excerpt
      FROM (SELECT convert_to(left(s.text, $3), 'UTF8') AS raw) bytes
      CROSS JOIN LATERAL (SELECT least($3, octet_length(raw)) AS n) cap
      CROSS JOIN generate_series(0, 3) AS backoff
      WHERE CASE WHEN n - backoff < 0 THEN false
                 WHEN n - backoff = octet_length(raw) THEN true
                 ELSE (get_byte(raw, n - backoff) & 192) <> 128 END
      ORDER BY backoff LIMIT 1) ex ON true
  WHERE u.rn <= $2 + $5),
kept AS (
  SELECT rn, item FROM (SELECT rn, item, sum(octet_length(item) + 1) OVER (ORDER BY rn) AS cum FROM items) z
  WHERE cum + 256 <= $4)
SELECT CASE WHEN $4 >= 256 THEN
  '{"results":[' || coalesce((SELECT string_agg(item, ',' ORDER BY rn) FROM kept), '') || '],"truncation":'
  || json_build_object('requested', $2 + $5, 'returned', (SELECT count(*) FROM kept),
       'truncated', (SELECT count(*) FROM uni) > $2 + $5 OR (SELECT count(*) FROM hits) > $2
                    OR (SELECT count(*) FROM nbr) > $5 OR (SELECT count(*) FROM edges) > $6
                    OR (SELECT count(*) FROM kept) < (SELECT count(*) FROM items),
       'underfilled', (SELECT count(*) FROM kept) < $2 + $5,
       'dropped_for_budget', (SELECT count(*) FROM items) - (SELECT count(*) FROM kept))::text || '}' END;

-- Resolve with the same verification: digest of the retained source and the
-- span text against its byte slice. verified = false marks corruption.
PREPARE baseline_resolve(uuid) AS
SELECT coalesce((
  SELECT json_build_object('status', CASE WHEN t.version_id IS NOT NULL THEN 'purged'
                   WHEN a.current_version_id = v.version_id THEN 'current'
                   WHEN p.version_id IS NULL THEN 'staged'
                   WHEN a.current_version_id IS NULL AND a.retired_at IS NOT NULL THEN 'retired'
                   ELSE 'historical' END,
    'evidence_id', s.evidence_id, 'version_id', s.version_id, 'path', v.path,
    'start_byte', s.start_byte, 'end_byte', s.end_byte,
    'text', CASE WHEN t.version_id IS NULL THEN s.text END,
    'verified', t.version_id IS NULL
      AND sha256(convert_to(v.source, 'UTF8')) = v.source_sha256
      AND substring(convert_to(v.source, 'UTF8') FROM s.start_byte + 1 FOR s.end_byte - s.start_byte)
          = convert_to(s.text, 'UTF8'))::text
  FROM :"corpus".spans s JOIN :"corpus".versions v ON v.version_id = s.version_id
  LEFT JOIN :"corpus".assets a ON a.asset_id = v.asset_id
  LEFT JOIN :"corpus".publications p ON p.version_id = v.version_id
  LEFT JOIN :"corpus".tombstones t ON t.version_id = v.version_id
  WHERE s.evidence_id = $1), '{"status":"not_found"}');

\if :{?dims}
PREPARE baseline_attach(uuid[], text[]) AS
INSERT INTO :"corpus".embeddings (evidence_id, embedding)
SELECT r.id, CAST(r.v AS :"vector_schema".vector(:dims))
FROM ROWS FROM (unnest($1), unnest($2)) AS r(id, v);

-- $1 query vector literal '[..]', $2 limit, $3 excerpt_bytes, $4 max_response_bytes.
PREPARE baseline_semantic(text, int, int, int) AS
WITH hits AS MATERIALIZED (
  SELECT q.evidence_id, q.score, row_number() OVER (ORDER BY q.score, q.evidence_id) AS rn
  FROM (SELECT s.evidence_id,
               e.embedding OPERATOR(:"vector_schema".<=>) CAST($1 AS :"vector_schema".vector(:dims)) AS score
        FROM :"corpus".embeddings e
        JOIN :"corpus".spans s ON s.evidence_id = e.evidence_id
        JOIN :"corpus".assets a ON a.current_version_id = s.version_id
        ORDER BY e.embedding OPERATOR(:"vector_schema".<=>) CAST($1 AS :"vector_schema".vector(:dims))
        LIMIT $2 + 1) q),
items AS (
  SELECT h.rn, json_build_object('evidence_id', s.evidence_id, 'version_id', s.version_id,
    'asset_id', v.asset_id, 'path', v.path, 'start_byte', s.start_byte, 'end_byte', s.end_byte,
    'status', 'current', 'mode', 'semantic', 'excerpt', ex.excerpt,
    'excerpt_truncated', octet_length(ex.excerpt) < s.end_byte - s.start_byte,
    'distance', h.score, 'approximate', true)::text AS item
  FROM hits h JOIN :"corpus".spans s ON s.evidence_id = h.evidence_id
  JOIN :"corpus".versions v ON v.version_id = s.version_id
  CROSS JOIN LATERAL (SELECT convert_from(substring(raw FROM 1 FOR n - backoff), 'UTF8') AS excerpt
      FROM (SELECT convert_to(left(s.text, $3), 'UTF8') AS raw) bytes
      CROSS JOIN LATERAL (SELECT least($3, octet_length(raw)) AS n) cap
      CROSS JOIN generate_series(0, 3) AS backoff
      WHERE CASE WHEN n - backoff < 0 THEN false
                 WHEN n - backoff = octet_length(raw) THEN true
                 ELSE (get_byte(raw, n - backoff) & 192) <> 128 END
      ORDER BY backoff LIMIT 1) ex
  WHERE h.rn <= $2),
kept AS (
  SELECT rn, item FROM (SELECT rn, item, sum(octet_length(item) + 1) OVER (ORDER BY rn) AS cum FROM items) z
  WHERE cum + 256 <= $4)
SELECT CASE WHEN $4 >= 256 THEN
  '{"results":[' || coalesce((SELECT string_agg(item, ',' ORDER BY rn) FROM kept), '') || '],"truncation":'
  || json_build_object('requested', $2, 'returned', (SELECT count(*) FROM kept),
       'truncated', (SELECT count(*) FROM hits) > $2 OR (SELECT count(*) FROM kept) < (SELECT count(*) FROM items),
       'underfilled', (SELECT count(*) FROM kept) < $2,
       'dropped_for_budget', (SELECT count(*) FROM items) - (SELECT count(*) FROM kept))::text || '}' END;
\endif
COMMIT;
