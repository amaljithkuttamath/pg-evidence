//! Canonical corpus DDL (schema_version 1), executed by evidence.init_collection
//! as the calling role, outside CREATE EXTENSION. Nothing here references an
//! object of the pg_evidence extension: no triggers, domains, types, defaults
//! or check functions from `evidence`. The only extension dependency is the
//! pgvector column, present only when embeddings are configured.
//! baseline/schema.sql must stay identical (checked by tests/system).

use crate::plan::{quote_ident, quote_literal};

const EQ: &str = "OPERATOR(pg_catalog.=)";
const GE: &str = "OPERATOR(pg_catalog.>=)";
const LE: &str = "OPERATOR(pg_catalog.<=)";
const LT: &str = "OPERATOR(pg_catalog.<)";
const NE: &str = "OPERATOR(pg_catalog.<>)";
const MINUS: &str = "OPERATOR(pg_catalog.-)";

/// `ts_config` is the schema-qualified, identifier-quoted regconfig name;
/// `vector` is (pgvector schema, dimensions) when embeddings are configured.
pub fn corpus_ddl(corpus: &str, ts_config: &str, vector: Option<(&str, u32)>) -> Vec<String> {
    let c = quote_ident(corpus);
    let ts = quote_literal(ts_config);
    let mut ddl = vec![
        format!("CREATE SCHEMA {c}"),
        format!(
            "CREATE TABLE {c}.\"collection_config\" (\
             singleton boolean PRIMARY KEY DEFAULT true CHECK (singleton), \
             schema_version integer NOT NULL, \
             embedding_model text, \
             embedding_dimensions integer, \
             text_search_config text NOT NULL, \
             max_source_bytes integer NOT NULL, \
             max_response_bytes integer NOT NULL, \
             max_candidates_per_operator integer NOT NULL, \
             max_plan_nodes integer NOT NULL, \
             max_edges_returned integer NOT NULL, \
             created_at timestamptz NOT NULL DEFAULT pg_catalog.now())"
        ),
        format!(
            "CREATE TABLE {c}.\"assets\" (\
             asset_id uuid PRIMARY KEY, \
             current_version_id uuid, \
             current_path text UNIQUE, \
             content_revision bigint NOT NULL DEFAULT 0 CHECK (content_revision {GE} 0), \
             annotation_revision bigint NOT NULL DEFAULT 0 CHECK (annotation_revision {GE} 0), \
             retired_at timestamptz, \
             created_at timestamptz NOT NULL DEFAULT pg_catalog.now(), \
             CHECK ((current_version_id IS NULL) {EQ} (current_path IS NULL)))"
        ),
        format!(
            "CREATE TABLE {c}.\"versions\" (\
             version_id uuid PRIMARY KEY, \
             asset_id uuid NOT NULL REFERENCES {c}.\"assets\" (asset_id), \
             path text NOT NULL, \
             source text, \
             source_sha256 bytea NOT NULL CHECK (pg_catalog.octet_length(source_sha256) {EQ} 32), \
             byte_length integer NOT NULL CHECK (byte_length {GE} 0), \
             ingestion_key text NOT NULL UNIQUE, \
             request_sha256 bytea NOT NULL, \
             base_revision bigint NOT NULL CHECK (base_revision {GE} 0), \
             created_by name NOT NULL DEFAULT CURRENT_USER, \
             created_at timestamptz NOT NULL DEFAULT pg_catalog.now(), \
             UNIQUE (asset_id, version_id), \
             CONSTRAINT versions_source_digest CHECK (source IS NULL OR (\
             source_sha256 {EQ} pg_catalog.sha256(pg_catalog.convert_to(source, 'UTF8')) \
             AND byte_length {EQ} pg_catalog.octet_length(source))))"
        ),
        format!(
            "CREATE TABLE {c}.\"publications\" (\
             asset_id uuid NOT NULL, \
             version_id uuid NOT NULL UNIQUE, \
             revision bigint NOT NULL CHECK (revision {GE} 1), \
             published_by name NOT NULL DEFAULT CURRENT_USER, \
             published_at timestamptz NOT NULL DEFAULT pg_catalog.now(), \
             PRIMARY KEY (asset_id, version_id), \
             UNIQUE (asset_id, revision), \
             FOREIGN KEY (asset_id, version_id) REFERENCES {c}.\"versions\" (asset_id, version_id))"
        ),
        format!(
            "ALTER TABLE {c}.\"assets\" ADD CONSTRAINT assets_current_publication \
             FOREIGN KEY (asset_id, current_version_id) REFERENCES {c}.\"publications\" (asset_id, version_id)"
        ),
        format!(
            "CREATE TABLE {c}.\"spans\" (\
             evidence_id uuid PRIMARY KEY, \
             version_id uuid NOT NULL REFERENCES {c}.\"versions\" (version_id), \
             start_byte integer NOT NULL CHECK (start_byte {GE} 0), \
             end_byte integer NOT NULL, \
             text text, \
             tsv tsvector GENERATED ALWAYS AS (pg_catalog.to_tsvector({ts}::pg_catalog.regconfig, text)) STORED, \
             UNIQUE (version_id, start_byte, end_byte), \
             CHECK (start_byte {LT} end_byte), \
             CHECK (text IS NULL OR pg_catalog.octet_length(text) {EQ} (end_byte {MINUS} start_byte)))"
        ),
        format!("CREATE INDEX spans_tsv_idx ON {c}.\"spans\" USING gin (tsv)"),
    ];
    if let Some((vs, dims)) = vector {
        let vs = quote_ident(vs);
        ddl.push(format!(
            "CREATE TABLE {c}.\"embeddings\" (\
             evidence_id uuid PRIMARY KEY REFERENCES {c}.\"spans\" (evidence_id), \
             embedding {vs}.vector({dims}) NOT NULL)"
        ));
        ddl.push(format!(
            "CREATE INDEX embeddings_hnsw_idx ON {c}.\"embeddings\" USING hnsw (embedding {vs}.vector_cosine_ops)"
        ));
    }
    ddl.extend([
        format!(
            "CREATE TABLE {c}.\"tags\" (\
             asset_id uuid NOT NULL REFERENCES {c}.\"assets\" (asset_id), \
             tag text NOT NULL CHECK (pg_catalog.octet_length(tag) {GE} 1 AND pg_catalog.octet_length(tag) {LE} 128), \
             created_by name NOT NULL DEFAULT CURRENT_USER, \
             created_at timestamptz NOT NULL DEFAULT pg_catalog.now(), \
             PRIMARY KEY (asset_id, tag))"
        ),
        format!("CREATE INDEX tags_tag_idx ON {c}.\"tags\" (tag, asset_id)"),
        format!(
            "CREATE TABLE {c}.\"relations\" (\
             source_evidence_id uuid NOT NULL REFERENCES {c}.\"spans\" (evidence_id), \
             kind text NOT NULL, \
             target_evidence_id uuid NOT NULL REFERENCES {c}.\"spans\" (evidence_id), \
             asserted_by name NOT NULL DEFAULT CURRENT_USER, \
             asserted_at timestamptz NOT NULL DEFAULT pg_catalog.now(), \
             PRIMARY KEY (source_evidence_id, kind, target_evidence_id), \
             CHECK (source_evidence_id {NE} target_evidence_id))"
        ),
        format!("CREATE INDEX relations_target_idx ON {c}.\"relations\" (target_evidence_id, kind)"),
        format!(
            "CREATE TABLE {c}.\"tombstones\" (\
             version_id uuid PRIMARY KEY REFERENCES {c}.\"versions\" (version_id), \
             purged_by name NOT NULL DEFAULT CURRENT_USER, \
             purged_at timestamptz NOT NULL DEFAULT pg_catalog.now(), \
             reason text NOT NULL)"
        ),
    ]);
    ddl
}

#[cfg(test)]
mod tests {
    use super::*;

    fn joined(v: &[String]) -> String {
        v.join(";\n")
    }

    #[test]
    fn creates_contract_tables_without_extension_references() {
        let ddl = corpus_ddl("docs", "pg_catalog.english", None);
        let all = joined(&ddl);
        assert!(ddl[0].starts_with("CREATE SCHEMA \"docs\""), "{}", ddl[0]);
        for table in [
            "collection_config",
            "assets",
            "versions",
            "spans",
            "publications",
            "tags",
            "relations",
            "tombstones",
        ] {
            assert!(
                all.contains(&format!("CREATE TABLE \"docs\".\"{table}\"")),
                "{table}"
            );
        }
        assert!(
            !all.contains("embeddings"),
            "no embeddings table without configuration"
        );
        assert!(!all.contains(".vector(") && !all.contains("hnsw"));
        assert!(
            !all.to_lowercase().contains("evidence."),
            "must not reference the extension schema"
        );
        assert!(!all.contains("\"evidence\""));
        assert!(all.contains("pg_catalog.sha256(pg_catalog.convert_to(source, 'UTF8'))"));
        assert!(all.contains("'pg_catalog.english'::pg_catalog.regconfig"));
        assert!(all.contains("REFERENCES \"docs\".\"publications\" (asset_id, version_id)"));
        assert!(!all.contains("DEFERRABLE"));
        assert!(!all.contains("TRIGGER"));
    }

    #[test]
    fn embeddings_use_qualified_pgvector() {
        let all = joined(&corpus_ddl(
            "docs",
            "pg_catalog.simple",
            Some(("public", 384)),
        ));
        assert!(all.contains("CREATE TABLE \"docs\".\"embeddings\""));
        assert!(all.contains("embedding \"public\".vector(384) NOT NULL"));
        assert!(all.contains("USING hnsw (embedding \"public\".vector_cosine_ops)"));
    }

    #[test]
    fn operators_are_qualified() {
        let mut all = joined(&corpus_ddl(
            "docs",
            "pg_catalog.simple",
            Some(("public", 3)),
        ));
        for op in [">=", "<=", "<>", "<", "=", "-"] {
            all = all.replace(&format!("OPERATOR(pg_catalog.{op})"), "");
        }
        let bare: Vec<char> = all.chars().filter(|c| "=<>!~@".contains(*c)).collect();
        assert!(bare.is_empty(), "{bare:?}");
    }
}
