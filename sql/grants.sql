-- pg-evidence v0.1: canonical grants for one corpus (docs/design.md "Roles").
-- Run as the corpus owner after evidence.init_collection:
--   psql -v corpus=docs -v reader=evidence_reader -v writer=evidence_writer \
--        -v purger=evidence_purger -f sql/grants.sql
-- A writer never receives UPDATE or DELETE on retained bytes, digests, spans,
-- embeddings or publication records. Only the purger may null source/text,
-- delete embeddings and insert tombstones.
\set ON_ERROR_STOP on
SELECT pg_catalog.to_regclass(pg_catalog.format('%I.embeddings', :'corpus')) IS NOT NULL AS pgev_has_embeddings \gset
BEGIN;
GRANT USAGE ON SCHEMA :"corpus" TO :"reader";
GRANT SELECT ON ALL TABLES IN SCHEMA :"corpus" TO :"reader";

GRANT INSERT ON :"corpus".assets, :"corpus".versions, :"corpus".spans,
    :"corpus".publications, :"corpus".tags, :"corpus".relations TO :"writer";
GRANT UPDATE (current_version_id, current_path, content_revision,
    annotation_revision, retired_at) ON :"corpus".assets TO :"writer";
GRANT DELETE ON :"corpus".tags, :"corpus".relations TO :"writer";

GRANT UPDATE (source) ON :"corpus".versions TO :"purger";
GRANT UPDATE (text) ON :"corpus".spans TO :"purger";
GRANT INSERT ON :"corpus".tombstones TO :"purger";
\if :pgev_has_embeddings
GRANT INSERT ON :"corpus".embeddings TO :"writer";
GRANT DELETE ON :"corpus".embeddings TO :"purger";
\endif
COMMIT;
