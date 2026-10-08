-- Matched SQL baseline: the canonical corpus schema (schema_version 1) without
-- the pg_evidence extension. Statement for statement the same DDL as
-- src/ddl.rs, so both arms have identical tables, constraints and indexes;
-- tests/system/test_baseline.py compares the catalogs.
--
--   psql -v corpus=docs_sql [-v ts_config=pg_catalog.english] \
--        [-v vector_schema=public -v dims=384 [-v embedding_model=m]] -f baseline/schema.sql
--
-- ts_config is a schema-qualified, identifier-quoted regconfig name, as stored
-- by evidence.init_collection. Limits are the documented defaults.
\set ON_ERROR_STOP on
\if :{?ts_config}
\else
\set ts_config pg_catalog.simple
\endif
\if :{?embedding_model}
\else
\set embedding_model baseline
\endif

BEGIN;
CREATE SCHEMA :"corpus";
CREATE TABLE :"corpus"."collection_config" (singleton boolean PRIMARY KEY DEFAULT true CHECK (singleton), schema_version integer NOT NULL, embedding_model text, embedding_dimensions integer, text_search_config text NOT NULL, max_source_bytes integer NOT NULL, max_response_bytes integer NOT NULL, max_candidates_per_operator integer NOT NULL, max_plan_nodes integer NOT NULL, max_edges_returned integer NOT NULL, created_at timestamptz NOT NULL DEFAULT pg_catalog.now());
CREATE TABLE :"corpus"."assets" (asset_id uuid PRIMARY KEY, current_version_id uuid, current_path text UNIQUE, content_revision bigint NOT NULL DEFAULT 0 CHECK (content_revision OPERATOR(pg_catalog.>=) 0), annotation_revision bigint NOT NULL DEFAULT 0 CHECK (annotation_revision OPERATOR(pg_catalog.>=) 0), retired_at timestamptz, created_at timestamptz NOT NULL DEFAULT pg_catalog.now(), CHECK ((current_version_id IS NULL) OPERATOR(pg_catalog.=) (current_path IS NULL)));
CREATE TABLE :"corpus"."versions" (version_id uuid PRIMARY KEY, asset_id uuid NOT NULL REFERENCES :"corpus"."assets" (asset_id), path text NOT NULL, source text, source_sha256 bytea NOT NULL CHECK (pg_catalog.octet_length(source_sha256) OPERATOR(pg_catalog.=) 32), byte_length integer NOT NULL CHECK (byte_length OPERATOR(pg_catalog.>=) 0), ingestion_key text NOT NULL UNIQUE, request_sha256 bytea NOT NULL, base_revision bigint NOT NULL CHECK (base_revision OPERATOR(pg_catalog.>=) 0), created_by name NOT NULL DEFAULT CURRENT_USER, created_at timestamptz NOT NULL DEFAULT pg_catalog.now(), UNIQUE (asset_id, version_id), CONSTRAINT versions_source_digest CHECK (source IS NULL OR (source_sha256 OPERATOR(pg_catalog.=) pg_catalog.sha256(pg_catalog.convert_to(source, 'UTF8')) AND byte_length OPERATOR(pg_catalog.=) pg_catalog.octet_length(source))));
CREATE TABLE :"corpus"."publications" (asset_id uuid NOT NULL, version_id uuid NOT NULL UNIQUE, revision bigint NOT NULL CHECK (revision OPERATOR(pg_catalog.>=) 1), published_by name NOT NULL DEFAULT CURRENT_USER, published_at timestamptz NOT NULL DEFAULT pg_catalog.now(), PRIMARY KEY (asset_id, version_id), UNIQUE (asset_id, revision), FOREIGN KEY (asset_id, version_id) REFERENCES :"corpus"."versions" (asset_id, version_id));
ALTER TABLE :"corpus"."assets" ADD CONSTRAINT assets_current_publication FOREIGN KEY (asset_id, current_version_id) REFERENCES :"corpus"."publications" (asset_id, version_id);
CREATE TABLE :"corpus"."spans" (evidence_id uuid PRIMARY KEY, version_id uuid NOT NULL REFERENCES :"corpus"."versions" (version_id), start_byte integer NOT NULL CHECK (start_byte OPERATOR(pg_catalog.>=) 0), end_byte integer NOT NULL, text text, tsv tsvector GENERATED ALWAYS AS (pg_catalog.to_tsvector(:'ts_config'::pg_catalog.regconfig, text)) STORED, UNIQUE (version_id, start_byte, end_byte), CHECK (start_byte OPERATOR(pg_catalog.<) end_byte), CHECK (text IS NULL OR pg_catalog.octet_length(text) OPERATOR(pg_catalog.=) (end_byte OPERATOR(pg_catalog.-) start_byte)));
CREATE INDEX spans_tsv_idx ON :"corpus"."spans" USING gin (tsv);
\if :{?dims}
CREATE TABLE :"corpus"."embeddings" (evidence_id uuid PRIMARY KEY REFERENCES :"corpus"."spans" (evidence_id), embedding :"vector_schema".vector(:dims) NOT NULL);
CREATE INDEX embeddings_hnsw_idx ON :"corpus"."embeddings" USING hnsw (embedding :"vector_schema".vector_cosine_ops);
\endif
CREATE TABLE :"corpus"."tags" (asset_id uuid NOT NULL REFERENCES :"corpus"."assets" (asset_id), tag text NOT NULL CHECK (pg_catalog.octet_length(tag) OPERATOR(pg_catalog.>=) 1 AND pg_catalog.octet_length(tag) OPERATOR(pg_catalog.<=) 128), created_by name NOT NULL DEFAULT CURRENT_USER, created_at timestamptz NOT NULL DEFAULT pg_catalog.now(), PRIMARY KEY (asset_id, tag));
CREATE INDEX tags_tag_idx ON :"corpus"."tags" (tag, asset_id);
CREATE TABLE :"corpus"."relations" (source_evidence_id uuid NOT NULL REFERENCES :"corpus"."spans" (evidence_id), kind text NOT NULL, target_evidence_id uuid NOT NULL REFERENCES :"corpus"."spans" (evidence_id), asserted_by name NOT NULL DEFAULT CURRENT_USER, asserted_at timestamptz NOT NULL DEFAULT pg_catalog.now(), PRIMARY KEY (source_evidence_id, kind, target_evidence_id), CHECK (source_evidence_id OPERATOR(pg_catalog.<>) target_evidence_id));
CREATE INDEX relations_target_idx ON :"corpus"."relations" (target_evidence_id, kind);
CREATE TABLE :"corpus"."tombstones" (version_id uuid PRIMARY KEY REFERENCES :"corpus"."versions" (version_id), purged_by name NOT NULL DEFAULT CURRENT_USER, purged_at timestamptz NOT NULL DEFAULT pg_catalog.now(), reason text NOT NULL);
\if :{?dims}
INSERT INTO :"corpus"."collection_config" (schema_version, embedding_model, embedding_dimensions, text_search_config, max_source_bytes, max_response_bytes, max_candidates_per_operator, max_plan_nodes, max_edges_returned) VALUES (1, :'embedding_model', :dims, :'ts_config', 1048576, 65536, 256, 32, 1024);
\else
INSERT INTO :"corpus"."collection_config" (schema_version, embedding_model, embedding_dimensions, text_search_config, max_source_bytes, max_response_bytes, max_candidates_per_operator, max_plan_nodes, max_edges_returned) VALUES (1, NULL, NULL, :'ts_config', 1048576, 65536, 256, 32, 1024);
\endif
COMMIT;
