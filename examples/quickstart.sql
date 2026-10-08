-- pg-evidence quickstart: psql -X -v ON_ERROR_STOP=1 -f examples/quickstart.sql
-- Creates corpus "quickstart", publishes two versions of one document, cites
-- the first, and shows that the citation still resolves after the edit.
-- Requires a UTF8 database where the caller may CREATE EXTENSION and schemas.
\set ON_ERROR_STOP on
CREATE EXTENSION IF NOT EXISTS pg_evidence;
SET statement_timeout = '5s';   -- evidence.query refuses to run without one

SELECT evidence.init_collection('quickstart', '{"text_search_config": "english"}');

\set asset '6f1c0a52-3b7e-4d0c-9a51-0c7d2b9e4a11'
\set src_a 'Retries are safe: a lost response replays the stored IDs.'
-- Spans are UTF-8 byte offsets; the digest is over the exact source bytes.
SELECT evidence.stage_version('quickstart', jsonb_build_object(
    'asset_id', :'asset', 'path', 'guide/retries.md', 'source', :'src_a',
    'source_sha256', encode(sha256(convert_to(:'src_a', 'UTF8')), 'hex'),
    'spans', jsonb_build_array(jsonb_build_object('start_byte', 0, 'end_byte', 17),
                               jsonb_build_object('start_byte', 18, 'end_byte', 57)),
    'ingestion_key', 'guide/retries.md@1', 'expected_revision', 0)) AS staged \gset
\echo :staged
SELECT (:'staged'::json)->>'version_id' AS v1,
       (:'staged'::json)->'spans'->1->>'evidence_id' AS citation \gset
SELECT evidence.publish_version('quickstart', jsonb_build_object('version_id', :'v1'));

\set src_b 'Retries are safe: an identical retry returns the stored IDs.'
SELECT (evidence.stage_version('quickstart', jsonb_build_object(
    'asset_id', :'asset', 'path', 'guide/retries.md', 'source', :'src_b',
    'source_sha256', encode(sha256(convert_to(:'src_b', 'UTF8')), 'hex'),
    'spans', jsonb_build_array(jsonb_build_object('start_byte', 18, 'end_byte', 60)),
    'ingestion_key', 'guide/retries.md@2', 'expected_revision', 1)))->>'version_id' AS v2 \gset
SELECT evidence.publish_version('quickstart', jsonb_build_object('version_id', :'v2'));

-- Current retrieval sees only version 2; the old citation resolves version 1.
SELECT jsonb_pretty(evidence.query('quickstart', '{
  "nodes": [{"id": "hits", "op": "lexical", "query": "stored IDs", "limit": 5}],
  "output": "hits", "excerpt_bytes": 80}')::jsonb);
SELECT jsonb_pretty(evidence.resolve('quickstart', :'citation'::uuid)::jsonb);

-- Tags scope the asset (own revision); relations bind exact evidence IDs.
SELECT evidence.annotate('quickstart', jsonb_build_object('action', 'tag', 'asset_id', :'asset',
    'tags', jsonb_build_array('guide'), 'expected_annotation_revision', 0));
