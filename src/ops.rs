//! Mutating API (docs/api.md). Each function is one SQL call: an ERROR rolls
//! back every write the call made. Calls take at most one explicit asset row
//! lock. Step order follows docs/design.md "Mutations, retries and concurrency".

use crate::db::{
    load_collection, require_utf8, vector_schema_sql, write_exec, write_json, Collection, P,
};
use crate::ddl::corpus_ddl;
use crate::error::{internal, invalid, precondition, unique, ApiError, ApiResult, SqlState};
use crate::model::{
    hex, parse_annotate, parse_attach, parse_config, parse_publish, parse_purge, parse_retire,
    parse_stage, validate_corpus_name, Annotate, StageRequest, Uuid16, SCHEMA_VERSION,
};
use crate::plan::{quote_ident, Tables};
use crate::render::{fit_response, JsonObject};
use serde_json::Value;
use std::collections::HashMap;

const EQ: &str = "OPERATOR(pg_catalog.=)";

fn malformed(what: &str) -> ApiError {
    internal(format!("unexpected {what} row"))
}

fn field<'a>(row: &'a [Value], i: usize, what: &str) -> ApiResult<&'a Value> {
    row.get(i).ok_or_else(|| malformed(what))
}

fn str_at<'a>(row: &'a [Value], i: usize, what: &str) -> ApiResult<&'a str> {
    field(row, i, what)?.as_str().ok_or_else(|| malformed(what))
}

fn int_at(row: &[Value], i: usize, what: &str) -> ApiResult<i64> {
    field(row, i, what)?.as_i64().ok_or_else(|| malformed(what))
}

fn row(v: Option<Value>, what: &str) -> ApiResult<Vec<Value>> {
    match v {
        Some(Value::Array(a)) => Ok(a),
        _ => Err(malformed(what)),
    }
}

fn unknown(reason: &str, field: &str, message: String) -> ApiError {
    ApiError::new(SqlState::InvalidParameter, reason, message).with("field", field)
}

fn revision_conflict(what: &str, current: i64, expected: i64) -> ApiError {
    precondition(
        "revision_conflict",
        format!("{what} is {current}, not {expected}"),
    )
    .with("current_revision", current)
    .with("expected_revision", expected)
}

/// Writes the shared asset row and returns its asset_id. A row lock alone does
/// not invalidate a waiting REPEATABLE READ snapshot when purge or attachment
/// changes only child rows. This self-assignment forces stale waiters to receive
/// 40001 without changing either public revision counter.
fn lock_asset_of_version(t: &Tables, version_id: Uuid16) -> ApiResult<String> {
    let v = write_json(
        &format!(
            "UPDATE {assets} a SET annotation_revision = a.annotation_revision WHERE a.asset_id {EQ} \
             (SELECT v.asset_id FROM {versions} v WHERE v.version_id {EQ} $1::pg_catalog.uuid) \
             RETURNING pg_catalog.to_json(a.asset_id)",
            assets = t.assets,
            versions = t.versions
        ),
        vec![P::uuid(version_id)],
    )?;
    match v {
        Some(Value::String(s)) => Ok(s),
        _ => Err(unknown(
            "unknown_version",
            "version_id",
            format!("unknown version_id {version_id}"),
        )),
    }
}

/// Locks an asset row and returns (content_revision, annotation_revision).
fn lock_asset(t: &Tables, asset_id: Uuid16) -> ApiResult<(i64, i64)> {
    let v = write_json(
        &format!(
            "SELECT pg_catalog.json_build_array(a.content_revision, a.annotation_revision) \
             FROM {assets} a WHERE a.asset_id {EQ} $1::pg_catalog.uuid FOR UPDATE",
            assets = t.assets
        ),
        vec![P::uuid(asset_id)],
    )?;
    let Some(v) = v else {
        return Err(unknown(
            "unknown_asset",
            "asset_id",
            format!("unknown asset_id {asset_id}"),
        ));
    };
    let r = row(Some(v), "asset")?;
    Ok((int_at(&r, 0, "asset")?, int_at(&r, 1, "asset")?))
}

// ---------------------------------------------------------------------------
// init_collection

pub fn init_collection(corpus: &str, config: &Value) -> ApiResult<()> {
    require_utf8()?;
    validate_corpus_name(corpus)?;
    let cfg = parse_config(config)?;
    let vector_schema =
        write_json(vector_schema_sql(), vec![])?.and_then(|v| v.as_str().map(str::to_string));
    if vector_schema.as_deref() == Some(corpus) {
        return Err(invalid(
            "corpus",
            "the corpus name is the schema holding pgvector",
        ));
    }
    let vector = match &cfg.embedding {
        None => None,
        Some(e) => Some((
            vector_schema.as_deref().ok_or_else(|| {
                precondition(
                    "pgvector_missing",
                    "CREATE EXTENSION vector before configuring embeddings",
                )
            })?,
            e.dimensions,
        )),
    };
    // Resolve the configuration to a schema-qualified, quoted name. Unqualified
    // names resolve in pg_catalog only (search_path is pg_catalog, pg_temp).
    let (ts_schema, ts_name) = match cfg.text_search_config.split_once('.') {
        Some((s, n)) => (s.to_string(), n.to_string()),
        None => ("pg_catalog".to_string(), cfg.text_search_config.clone()),
    };
    let ts_config = write_json(
        "SELECT pg_catalog.to_json(pg_catalog.quote_ident(n.nspname::pg_catalog.text) OPERATOR(pg_catalog.||) '.' \
         OPERATOR(pg_catalog.||) pg_catalog.quote_ident(c.cfgname::pg_catalog.text)) \
         FROM pg_catalog.pg_ts_config c JOIN pg_catalog.pg_namespace n ON n.oid OPERATOR(pg_catalog.=) c.cfgnamespace \
         WHERE n.nspname OPERATOR(pg_catalog.=) $1::pg_catalog.name AND c.cfgname OPERATOR(pg_catalog.=) $2::pg_catalog.name",
        vec![P::Text(ts_schema), P::Text(ts_name)],
    )?
    .and_then(|v| v.as_str().map(str::to_string))
    .ok_or_else(|| {
        invalid(
            "config.text_search_config",
            format!("unknown text search configuration \"{}\"", cfg.text_search_config),
        )
    })?;

    // CREATE SCHEMA raises PostgreSQL's own 42P06 if the corpus exists.
    for statement in corpus_ddl(corpus, &ts_config, vector) {
        write_exec(&statement, vec![])?;
    }
    let l = cfg.limits;
    write_exec(
        &format!(
            "INSERT INTO {c}.\"collection_config\" (schema_version, embedding_model, embedding_dimensions, \
             text_search_config, max_source_bytes, max_response_bytes, max_candidates_per_operator, \
             max_plan_nodes, max_edges_returned) VALUES ($1, $2, $3, $4, $5, $6, $7, $8, $9)",
            c = quote_ident(corpus)
        ),
        vec![
            P::Int4(SCHEMA_VERSION),
            P::OptText(cfg.embedding.as_ref().map(|e| e.model.clone())),
            P::OptInt4(cfg.embedding.as_ref().map(|e| e.dimensions as i32)),
            P::Text(ts_config),
            P::Int4(l.max_source_bytes as i32),
            P::Int4(l.max_response_bytes as i32),
            P::Int4(l.max_candidates_per_operator as i32),
            P::Int4(l.max_plan_nodes as i32),
            P::Int4(l.max_edges_returned as i32),
        ],
    )?;
    Ok(())
}

// ---------------------------------------------------------------------------
// stage_version

/// [version_id, request_sha256, asset_id, path, source_sha256, byte_length,
///  base_revision, [[evidence_id, start_byte, end_byte], ...]]
fn find_by_key(t: &Tables, key: &str) -> ApiResult<Option<Vec<Value>>> {
    let v = write_json(
        &format!(
            "SELECT pg_catalog.json_build_array(v.version_id, pg_catalog.encode(v.request_sha256, 'hex'), \
             v.asset_id, v.path, pg_catalog.encode(v.source_sha256, 'hex'), v.byte_length, v.base_revision, \
             (SELECT pg_catalog.json_agg(pg_catalog.json_build_array(s.evidence_id, s.start_byte, s.end_byte)) \
             FROM {spans} s WHERE s.version_id {EQ} v.version_id)) \
             FROM {versions} v WHERE v.ingestion_key {EQ} $1",
            spans = t.spans,
            versions = t.versions
        ),
        vec![P::Text(key.to_string())],
    )?;
    v.map(|v| row(Some(v), "version")).transpose()
}

struct Staged<'a> {
    version_id: &'a str,
    asset_id: &'a str,
    path: &'a str,
    source_sha256: &'a str,
    byte_length: i64,
    base_revision: i64,
    replayed: bool,
}

/// Renders the stage response with evidence IDs in request span order.
fn render_stage(s: Staged, req: &StageRequest, ids: &Value) -> ApiResult<String> {
    let mut by_span: HashMap<(i64, i64), &str> = HashMap::new();
    if let Some(rows) = ids.as_array() {
        for r in rows {
            let a = r.as_array().ok_or_else(|| malformed("span"))?;
            by_span.insert(
                (int_at(a, 1, "span")?, int_at(a, 2, "span")?),
                str_at(a, 0, "span")?,
            );
        }
    }
    let mut spans = String::from("[");
    for (i, span) in req.spans.iter().enumerate() {
        pgrx::check_for_interrupts!();
        let id = by_span
            .get(&(span.start as i64, span.end as i64))
            .ok_or_else(|| internal("stored spans do not match the request"))?;
        let mut o = JsonObject::new();
        o.str("evidence_id", id)
            .int("start_byte", span.start as i64)
            .int("end_byte", span.end as i64);
        if i > 0 {
            spans.push(',');
        }
        spans.push_str(&o.finish());
    }
    spans.push(']');
    let mut o = JsonObject::new();
    o.str("status", "staged")
        .bool("replayed", s.replayed)
        .str("version_id", s.version_id)
        .str("asset_id", s.asset_id)
        .str("path", s.path)
        .str("source_sha256", s.source_sha256)
        .int("byte_length", s.byte_length)
        .int("base_revision", s.base_revision)
        .raw("spans", &spans);
    Ok(o.finish())
}

/// Step 2: the same canonical request returns the stored IDs without any
/// other check; a different request under the key is 23505.
fn replay(stored: &[Value], req: &StageRequest, digest: &str) -> ApiResult<String> {
    if str_at(stored, 1, "version")? != digest {
        return Err(unique(
            "ingestion_key_reused",
            "ingestion_key was already used for a different request",
        )
        .with("version_id", str_at(stored, 0, "version")?));
    }
    render_stage(
        Staged {
            version_id: str_at(stored, 0, "version")?,
            asset_id: str_at(stored, 2, "version")?,
            path: str_at(stored, 3, "version")?,
            source_sha256: str_at(stored, 4, "version")?,
            byte_length: int_at(stored, 5, "version")?,
            base_revision: int_at(stored, 6, "version")?,
            replayed: true,
        },
        req,
        field(stored, 7, "version")?,
    )
}

fn stage_version_body(col: &Collection, corpus: &str, request: &Value) -> ApiResult<String> {
    let req = parse_stage(request, col.config.limits.max_source_bytes)?;
    let digest = hex(&req.request_sha256());
    let t = Tables::new(corpus);
    // The response size is known before any write: IDs are fixed-length UUIDs.
    let placeholder = "00000000-0000-0000-0000-000000000000";
    let placeholder_ids = Value::Array(
        req.spans
            .iter()
            .map(|s| serde_json::json!([placeholder, s.start, s.end]))
            .collect(),
    );
    fit_response(
        render_stage(
            Staged {
                version_id: placeholder,
                asset_id: placeholder,
                path: &req.path,
                source_sha256: &hex(&req.source_sha256),
                byte_length: req.source.len() as i64,
                base_revision: req.expected_revision,
                replayed: false,
            },
            &req,
            &placeholder_ids,
        )?,
        col.config.limits.max_response_bytes,
    )?;

    if let Some(stored) = find_by_key(&t, &req.ingestion_key)? {
        return replay(&stored, &req, &digest);
    }

    let inserted_asset = write_json(
        &format!(
            "INSERT INTO {assets} (asset_id) VALUES ($1::pg_catalog.uuid) ON CONFLICT (asset_id) DO NOTHING \
             RETURNING pg_catalog.to_json(true)",
            assets = t.assets
        ),
        vec![P::uuid(req.asset_id)],
    )?
    .is_some();
    if inserted_asset && req.expected_revision != 0 {
        // The error rolls back the asset row inserted above.
        return Err(revision_conflict(
            "content_revision of a new asset",
            0,
            req.expected_revision,
        ));
    }

    let source_sha256 = hex(&req.source_sha256);
    let inserted = write_json(
        &format!(
            "INSERT INTO {versions} (version_id, asset_id, path, source, source_sha256, byte_length, \
             ingestion_key, request_sha256, base_revision) VALUES (pg_catalog.gen_random_uuid(), \
             $1::pg_catalog.uuid, $2, $3, pg_catalog.decode($4, 'hex'), $5, $6, pg_catalog.decode($7, 'hex'), $8) \
             ON CONFLICT (ingestion_key) DO NOTHING RETURNING pg_catalog.to_json(version_id)",
            versions = t.versions
        ),
        vec![
            P::uuid(req.asset_id),
            P::Text(req.path.clone()),
            P::Text(req.source.clone()),
            P::Text(source_sha256.clone()),
            P::Int4(req.source.len() as i32),
            P::Text(req.ingestion_key.clone()),
            P::Text(digest.clone()),
            P::Int8(req.expected_revision),
        ],
    )?;
    let Some(Value::String(version_id)) = inserted else {
        // A concurrent request with this key committed first: apply step 2.
        let stored = find_by_key(&t, &req.ingestion_key)?
            .ok_or_else(|| internal("ingestion_key conflict without a visible version"))?;
        return replay(&stored, &req, &digest);
    };

    let ids = if req.spans.is_empty() {
        Value::Null
    } else {
        write_json(
            &format!(
                "WITH ins AS (INSERT INTO {spans} (evidence_id, version_id, start_byte, end_byte, text) \
                 SELECT pg_catalog.gen_random_uuid(), $1::pg_catalog.uuid, u.s, u.e, u.t \
                 FROM ROWS FROM (pg_catalog.unnest($2::pg_catalog.int4[]), pg_catalog.unnest($3::pg_catalog.int4[]), \
                 pg_catalog.unnest($4::pg_catalog.text[])) AS u(s, e, t) \
                 RETURNING evidence_id, start_byte, end_byte) \
                 SELECT pg_catalog.json_agg(pg_catalog.json_build_array(evidence_id, start_byte, end_byte)) FROM ins",
                spans = t.spans
            ),
            vec![
                P::Text(version_id.clone()),
                P::Int4Array(req.spans.iter().map(|s| s.start).collect()),
                P::Int4Array(req.spans.iter().map(|s| s.end).collect()),
                P::TextArray(req.spans.iter().map(|s| req.span_text(*s).to_string()).collect()),
            ],
        )?
        .unwrap_or(Value::Null)
    };
    render_stage(
        Staged {
            version_id: &version_id,
            asset_id: &req.asset_id.to_string(),
            path: &req.path,
            source_sha256: &source_sha256,
            byte_length: req.source.len() as i64,
            base_revision: req.expected_revision,
            replayed: false,
        },
        &req,
        &ids,
    )
}

// ---------------------------------------------------------------------------
// attach_embeddings

/// (purged, published) for a version whose asset row is locked.
fn version_state(t: &Tables, version_id: Uuid16) -> ApiResult<(bool, bool)> {
    let r = row(
        write_json(
            &format!(
                "SELECT pg_catalog.json_build_array(\
                 EXISTS (SELECT 1 FROM {tombstones} x WHERE x.version_id {EQ} $1::pg_catalog.uuid), \
                 EXISTS (SELECT 1 FROM {publications} p WHERE p.version_id {EQ} $1::pg_catalog.uuid))",
                tombstones = t.tombstones,
                publications = t.publications
            ),
            vec![P::uuid(version_id)],
        )?,
        "version state",
    )?;
    let b = |i| {
        field(&r, i, "version state")
            .and_then(|v| v.as_bool().ok_or_else(|| malformed("version state")))
    };
    Ok((b(0)?, b(1)?))
}

fn vector_literal(v: &[f32]) -> String {
    // Rust prints the shortest representation that round-trips through
    // strtof, so pgvector stores exactly these float4 values.
    let items: Vec<String> = v.iter().map(|f| f.to_string()).collect();
    format!("[{}]", items.join(","))
}

pub fn attach_embeddings(corpus: &str, request: &Value) -> ApiResult<()> {
    let col = load_collection(corpus, false)?;
    let (emb, vs) = col.vectors()?;
    let t = Tables::new(corpus);
    // The purge check precedes all other validation, so read version_id first.
    let version_id = match request.get("version_id") {
        Some(v) => Uuid16::from_value(v, "version_id")?,
        None => return Err(invalid("version_id", "version_id is required")),
    };
    lock_asset_of_version(&t, version_id)?;
    let (purged, published) = version_state(&t, version_id)?;
    if purged {
        return Err(precondition(
            "version_purged",
            format!("version {version_id} is purged"),
        ));
    }
    let req = parse_attach(request, emb)?;
    let version_id = req.version_id;
    let vsq = quote_ident(vs);
    let dims = emb.dimensions;
    let ids: Vec<String> = req.vectors.iter().map(|(id, _)| id.to_string()).collect();
    let literals: Vec<String> = req.vectors.iter().map(|(_, v)| vector_literal(v)).collect();
    // Per requested vector: [is a span of this version, already stored, stored value equal].
    let checks = row(
        write_json(
            &format!(
                "SELECT pg_catalog.json_agg(pg_catalog.json_build_array(s.evidence_id IS NOT NULL, \
                 e.evidence_id IS NOT NULL, COALESCE(e.embedding OPERATOR({vsq}.=) r.v::{vsq}.vector({dims}), false)) \
                 ORDER BY r.ord) \
                 FROM ROWS FROM (pg_catalog.unnest($2::pg_catalog.text[]), pg_catalog.unnest($3::pg_catalog.text[])) \
                 WITH ORDINALITY AS r(id, v, ord) \
                 LEFT JOIN {spans} s ON s.evidence_id {EQ} r.id::pg_catalog.uuid AND s.version_id {EQ} $1::pg_catalog.uuid \
                 LEFT JOIN {embeddings} e ON e.evidence_id {EQ} s.evidence_id",
                spans = t.spans,
                embeddings = t.embeddings
            ),
            vec![P::uuid(version_id), P::TextArray(ids.clone()), P::TextArray(literals.clone())],
        )?,
        "embedding check",
    )?;
    if checks.len() != ids.len() {
        return Err(malformed("embedding check"));
    }
    let flags: Vec<[bool; 3]> = checks
        .iter()
        .map(|c| {
            let a = c
                .as_array()
                .filter(|a| a.len() == 3)
                .ok_or_else(|| malformed("embedding check"))?;
            let b = |i: usize| a[i].as_bool().ok_or_else(|| malformed("embedding check"));
            Ok([b(0)?, b(1)?, b(2)?])
        })
        .collect::<ApiResult<_>>()?;
    for (i, f) in flags.iter().enumerate() {
        pgrx::check_for_interrupts!();
        if !f[0] {
            return Err(unknown(
                "evidence_not_in_version",
                &format!("embeddings[{i}].evidence_id"),
                format!("{} is not a span of version {version_id}", ids[i]),
            ));
        }
    }
    if let Some(i) = flags.iter().position(|f| f[1] && !f[2]) {
        return Err(unique(
            "embedding_conflict",
            format!("a different embedding is stored for {}", ids[i]),
        )
        .with("evidence_id", ids[i].as_str()));
    }
    let missing: Vec<usize> = (0..flags.len()).filter(|i| !flags[*i][1]).collect();
    if missing.is_empty() {
        return Ok(()); // every vector already stored identically: a retry
    }
    if published {
        return Err(precondition(
            "version_published",
            format!("version {version_id} is published; its embeddings cannot change"),
        ));
    }
    write_exec(
        &format!(
            "INSERT INTO {embeddings} (evidence_id, embedding) SELECT r.id::pg_catalog.uuid, r.v::{vsq}.vector({dims}) \
             FROM ROWS FROM (pg_catalog.unnest($1::pg_catalog.text[]), pg_catalog.unnest($2::pg_catalog.text[])) AS r(id, v)",
            embeddings = t.embeddings
        ),
        vec![
            P::TextArray(missing.iter().map(|i| ids[*i].clone()).collect()),
            P::TextArray(missing.iter().map(|i| literals[*i].clone()).collect()),
        ],
    )?;
    Ok(())
}

// ---------------------------------------------------------------------------
// publish_version and retire

fn publish_version_body(col: &Collection, corpus: &str, request: &Value) -> ApiResult<String> {
    let req = parse_publish(request)?;
    let t = Tables::new(corpus);
    let asset_id = lock_asset_of_version(&t, req.version_id)?;
    let missing = if col.config.embedding.is_some() {
        format!(
            "(SELECT pg_catalog.count(*) FROM {spans} s WHERE s.version_id {EQ} v.version_id AND NOT EXISTS \
             (SELECT 1 FROM {embeddings} e WHERE e.evidence_id {EQ} s.evidence_id))",
            spans = t.spans,
            embeddings = t.embeddings
        )
    } else {
        "0".to_string()
    };
    let st = row(
        write_json(
            &format!(
                "SELECT pg_catalog.json_build_array(\
                 (SELECT pg_catalog.json_build_array(p.revision, p.published_at, p.published_by) FROM {publications} p \
                 WHERE p.version_id {EQ} v.version_id), \
                 EXISTS (SELECT 1 FROM {tombstones} x WHERE x.version_id {EQ} v.version_id), \
                 v.base_revision, v.path, a.content_revision, \
                 COALESCE(a.current_version_id {EQ} v.version_id, false), {missing}) \
                 FROM {versions} v JOIN {assets} a ON a.asset_id {EQ} v.asset_id \
                 WHERE v.version_id {EQ} $1::pg_catalog.uuid",
                publications = t.publications,
                tombstones = t.tombstones,
                versions = t.versions,
                assets = t.assets
            ),
            vec![P::uuid(req.version_id)],
        )?,
        "publish state",
    )?;
    let what = "publish state";
    let path = str_at(&st, 3, what)?;
    let version_id = req.version_id.to_string();
    let render =
        |revision: i64, published_at: &str, published_by: &str, replayed: bool, current: bool| {
            let mut o = JsonObject::new();
            o.str("status", "published")
                .bool("replayed", replayed)
                .str("version_id", &version_id)
                .str("asset_id", &asset_id)
                .int("revision", revision)
                .str("path", path)
                .str("published_at", published_at)
                .str("published_by", published_by)
                .bool("current", current);
            o.finish()
        };

    if let Some(Value::Array(p)) = st.first() {
        // Publication is idempotent per version, even after supersession.
        let current = field(&st, 5, what)?.as_bool().unwrap_or(false);
        return Ok(render(
            int_at(p, 0, what)?,
            str_at(p, 1, what)?,
            str_at(p, 2, what)?,
            true,
            current,
        ));
    }
    if field(&st, 1, what)?.as_bool() == Some(true) {
        return Err(precondition(
            "version_purged",
            format!("version {version_id} is purged"),
        ));
    }
    let (base, content) = (int_at(&st, 2, what)?, int_at(&st, 4, what)?);
    if base != content {
        return Err(
            revision_conflict("the asset's content_revision", content, base)
                .with("base_revision", base)
                .with("version_id", version_id.as_str()),
        );
    }
    let missing = int_at(&st, 6, what)?;
    if missing > 0 {
        return Err(precondition(
            "embeddings_missing",
            format!("{missing} spans of version {version_id} lack embeddings"),
        )
        .with("missing", missing));
    }
    let revision = content + 1;
    let published = row(
        write_json(
            &format!(
                "INSERT INTO {publications} (asset_id, version_id, revision) \
                 VALUES ($1::pg_catalog.uuid, $2::pg_catalog.uuid, $3) \
                 RETURNING pg_catalog.json_build_array(published_at, published_by)",
                publications = t.publications
            ),
            vec![
                P::Text(asset_id.clone()),
                P::Text(version_id.clone()),
                P::Int8(revision),
            ],
        )?,
        "publication",
    )?;
    // The publication row exists before the pointer names it (immediate FK).
    // A current_path collision raises PostgreSQL's 23505 and rolls back both.
    write_exec(
        &format!(
            "UPDATE {assets} SET current_version_id = $2::pg_catalog.uuid, current_path = $3, \
             content_revision = $4, retired_at = NULL WHERE asset_id {EQ} $1::pg_catalog.uuid",
            assets = t.assets
        ),
        vec![
            P::Text(asset_id.clone()),
            P::Text(version_id.clone()),
            P::Text(path.to_string()),
            P::Int8(revision),
        ],
    )?;
    Ok(render(
        revision,
        str_at(&published, 0, "publication")?,
        str_at(&published, 1, "publication")?,
        false,
        true,
    ))
}

fn retire_body(_col: &Collection, corpus: &str, request: &Value) -> ApiResult<String> {
    let req = parse_retire(request)?;
    let t = Tables::new(corpus);
    let (content, _) = lock_asset(&t, req.asset_id)?;
    if content != req.expected_revision {
        return Err(revision_conflict(
            "the asset's content_revision",
            content,
            req.expected_revision,
        ));
    }
    let retired_at = write_json(
        &format!(
            "UPDATE {assets} SET current_version_id = NULL, current_path = NULL, retired_at = pg_catalog.now(), \
             content_revision = $2 WHERE asset_id {EQ} $1::pg_catalog.uuid RETURNING pg_catalog.to_json(retired_at)",
            assets = t.assets
        ),
        vec![P::uuid(req.asset_id), P::Int8(content + 1)],
    )?;
    let retired_at = retired_at
        .as_ref()
        .and_then(Value::as_str)
        .ok_or_else(|| malformed("retire"))?;
    let mut o = JsonObject::new();
    o.str("status", "retired")
        .str("asset_id", &req.asset_id.to_string())
        .int("content_revision", content + 1)
        .str("retired_at", retired_at);
    Ok(o.finish())
}

// ---------------------------------------------------------------------------
// annotate

fn string_array(v: Option<Value>) -> String {
    match v {
        Some(v @ Value::Array(_)) => v.to_string(),
        _ => "[]".to_string(),
    }
}

fn annotate_body(_col: &Collection, corpus: &str, request: &Value) -> ApiResult<String> {
    let action = parse_annotate(request)?;
    let t = Tables::new(corpus);
    let is_tag = matches!(action, Annotate::Tag { .. });
    match action {
        Annotate::Tag {
            asset_id,
            tags,
            expected_annotation_revision: expected,
        }
        | Annotate::Untag {
            asset_id,
            tags,
            expected_annotation_revision: expected,
        } => {
            let (_, current) = lock_asset(&t, asset_id)?;
            if current != expected {
                return Err(revision_conflict(
                    "the asset's annotation_revision",
                    current,
                    expected,
                ));
            }
            let changed = if is_tag {
                write_json(
                    &format!(
                        "WITH ins AS (INSERT INTO {tags} (asset_id, tag) SELECT $1::pg_catalog.uuid, x.tag \
                         FROM pg_catalog.unnest($2::pg_catalog.text[]) AS x(tag) \
                         ON CONFLICT (asset_id, tag) DO NOTHING RETURNING tag) \
                         SELECT pg_catalog.json_agg(ins.tag ORDER BY ins.tag) FROM ins",
                        tags = t.tags
                    ),
                    vec![P::uuid(asset_id), P::TextArray(tags)],
                )?
            } else {
                write_json(
                    &format!(
                        "WITH del AS (DELETE FROM {tags} g WHERE g.asset_id {EQ} $1::pg_catalog.uuid \
                         AND g.tag {EQ} ANY ($2::pg_catalog.text[]) RETURNING g.tag) \
                         SELECT pg_catalog.json_agg(del.tag ORDER BY del.tag) FROM del",
                        tags = t.tags
                    ),
                    vec![P::uuid(asset_id), P::TextArray(tags)],
                )?
            };
            write_exec(
                &format!(
                    "UPDATE {assets} SET annotation_revision = $2 WHERE asset_id {EQ} $1::pg_catalog.uuid",
                    assets = t.assets
                ),
                vec![P::uuid(asset_id), P::Int8(current + 1)],
            )?;
            let mut o = JsonObject::new();
            o.str("status", "annotated")
                .str("action", if is_tag { "tag" } else { "untag" })
                .str("asset_id", &asset_id.to_string())
                .int("annotation_revision", current + 1)
                .raw(
                    if is_tag { "added" } else { "removed" },
                    &string_array(changed),
                );
            Ok(o.finish())
        }
        Annotate::Link {
            source,
            target,
            kind,
        } => {
            let exists = row(
                write_json(
                    &format!(
                        "SELECT pg_catalog.json_build_array(\
                         EXISTS (SELECT 1 FROM {spans} s WHERE s.evidence_id {EQ} $1::pg_catalog.uuid), \
                         EXISTS (SELECT 1 FROM {spans} s WHERE s.evidence_id {EQ} $2::pg_catalog.uuid))",
                        spans = t.spans
                    ),
                    vec![P::uuid(source), P::uuid(target)],
                )?,
                "endpoints",
            )?;
            for (i, name, id) in [
                (0, "source_evidence_id", source),
                (1, "target_evidence_id", target),
            ] {
                if field(&exists, i, "endpoints")?.as_bool() != Some(true) {
                    return Err(unknown(
                        "unknown_evidence",
                        name,
                        format!("unknown evidence_id {id}"),
                    ));
                }
            }
            // INSERT .. DO NOTHING then SELECT can miss the row when a concurrent
            // unlink commits in between. Retry once (writers have INSERT/DELETE,
            // not UPDATE, on relations), then report a transient 55000.
            let params = || vec![P::uuid(source), P::Text(kind.clone()), P::uuid(target)];
            let mut found = None;
            for _attempt in 0..2 {
                pgrx::check_for_interrupts!();
                let inserted = write_json(
                    &format!(
                        "INSERT INTO {relations} (source_evidence_id, kind, target_evidence_id) \
                         VALUES ($1::pg_catalog.uuid, $2, $3::pg_catalog.uuid) \
                         ON CONFLICT (source_evidence_id, kind, target_evidence_id) DO NOTHING \
                         RETURNING pg_catalog.json_build_array(asserted_by, asserted_at)",
                        relations = t.relations
                    ),
                    params(),
                )?;
                if let Some(v) = inserted {
                    found = Some(("linked", row(Some(v), "relation")?));
                    break;
                }
                let existing = write_json(
                    &format!(
                        "SELECT pg_catalog.json_build_array(r.asserted_by, r.asserted_at) FROM {relations} r \
                         WHERE r.source_evidence_id {EQ} $1::pg_catalog.uuid AND r.kind {EQ} $2 \
                         AND r.target_evidence_id {EQ} $3::pg_catalog.uuid",
                        relations = t.relations
                    ),
                    params(),
                )?;
                if let Some(v) = existing {
                    found = Some(("existing", row(Some(v), "relation")?));
                    break;
                }
            }
            let Some((status, r)) = found else {
                return Err(precondition(
                    "relation_changed",
                    "the relation was concurrently removed while linking; retry the call",
                )
                .with("hint", "retry the link; a concurrent unlink won twice"));
            };
            let mut o = JsonObject::new();
            o.str("status", status)
                .str("source_evidence_id", &source.to_string())
                .str("kind", &kind)
                .str("target_evidence_id", &target.to_string())
                .str("asserted_by", str_at(&r, 0, "relation")?)
                .str("asserted_at", str_at(&r, 1, "relation")?);
            Ok(o.finish())
        }
        Annotate::Unlink {
            source,
            target,
            kind,
        } => {
            let deleted = write_exec(
                &format!(
                    "DELETE FROM {relations} r WHERE r.source_evidence_id {EQ} $1::pg_catalog.uuid AND r.kind {EQ} $2 \
                     AND r.target_evidence_id {EQ} $3::pg_catalog.uuid",
                    relations = t.relations
                ),
                vec![P::uuid(source), P::Text(kind.clone()), P::uuid(target)],
            )?;
            let mut o = JsonObject::new();
            o.str("status", if deleted > 0 { "unlinked" } else { "absent" })
                .str("source_evidence_id", &source.to_string())
                .str("kind", &kind)
                .str("target_evidence_id", &target.to_string());
            Ok(o.finish())
        }
    }
}

// ---------------------------------------------------------------------------
// purge

fn purge_body(col: &Collection, corpus: &str, request: &Value) -> ApiResult<String> {
    let req = parse_purge(request)?;
    let t = Tables::new(corpus);
    // Purge requires the purger role; check before any read that could replay.
    let allowed = write_json(
        "SELECT pg_catalog.to_json(pg_catalog.has_column_privilege($1, 'source', 'UPDATE'))",
        vec![P::Text(t.versions.clone())],
    )?;
    if allowed != Some(Value::Bool(true)) {
        return Err(ApiError::new(
            SqlState::InsufficientPrivilege,
            "purger_required",
            format!("purge requires UPDATE (source) on {}", t.versions),
        ));
    }
    let asset_id = lock_asset_of_version(&t, req.version_id)?;
    let version_id = req.version_id.to_string();
    let st = row(
        write_json(
            &format!(
                "SELECT pg_catalog.json_build_array(\
                 (SELECT pg_catalog.json_build_array(x.purged_at, x.reason, x.purged_by) FROM {tombstones} x \
                 WHERE x.version_id {EQ} $1::pg_catalog.uuid), \
                 COALESCE((SELECT a.current_version_id {EQ} $1::pg_catalog.uuid FROM {assets} a \
                 WHERE a.asset_id {EQ} $2::pg_catalog.uuid), false))",
                tombstones = t.tombstones,
                assets = t.assets
            ),
            vec![P::Text(version_id.clone()), P::Text(asset_id.clone())],
        )?,
        "purge state",
    )?;
    let render = |purged_at: &str,
                  reason: &str,
                  purged_by: &str,
                  replayed: bool,
                  spans: i64,
                  embeddings: i64| {
        let mut o = JsonObject::new();
        o.str("status", "purged")
            .bool("replayed", replayed)
            .str("version_id", &version_id)
            .str("asset_id", &asset_id)
            .str("purged_at", purged_at)
            .str("purged_by", purged_by)
            .str("reason", reason)
            .int("spans_purged", spans)
            .int("embeddings_deleted", embeddings);
        o.finish()
    };
    if let Some(Value::Array(x)) = st.first() {
        let what = "tombstone";
        return Ok(render(
            str_at(x, 0, what)?,
            str_at(x, 1, what)?,
            str_at(x, 2, what)?,
            true,
            0,
            0,
        ));
    }
    if field(&st, 1, "purge state")?.as_bool() == Some(true) {
        return Err(precondition(
            "version_current",
            format!("version {version_id} is current; publish another version or retire the asset first"),
        ));
    }
    write_exec(
        &format!(
            "UPDATE {versions} SET source = NULL WHERE version_id {EQ} $1::pg_catalog.uuid",
            versions = t.versions
        ),
        vec![P::Text(version_id.clone())],
    )?;
    let spans = write_exec(
        &format!(
            "UPDATE {spans} SET text = NULL WHERE version_id {EQ} $1::pg_catalog.uuid",
            spans = t.spans
        ),
        vec![P::Text(version_id.clone())],
    )?;
    let embeddings = if col.config.embedding.is_some() {
        write_exec(
            &format!(
                "DELETE FROM {embeddings} e WHERE e.evidence_id {EQ} ANY \
                 (SELECT s.evidence_id FROM {spans} s WHERE s.version_id {EQ} $1::pg_catalog.uuid)",
                embeddings = t.embeddings,
                spans = t.spans
            ),
            vec![P::Text(version_id.clone())],
        )?
    } else {
        0
    };
    let tomb = row(
        write_json(
            &format!(
                "INSERT INTO {tombstones} (version_id, reason) VALUES ($1::pg_catalog.uuid, $2) \
                 RETURNING pg_catalog.json_build_array(purged_at, reason, purged_by)",
                tombstones = t.tombstones
            ),
            vec![P::Text(version_id.clone()), P::Text(req.reason.clone())],
        )?,
        "tombstone",
    )?;
    Ok(render(
        str_at(&tomb, 0, "tombstone")?,
        str_at(&tomb, 1, "tombstone")?,
        str_at(&tomb, 2, "tombstone")?,
        false,
        spans as i64,
        embeddings as i64,
    ))
}

// Public entry points: every mutation response obeys max_response_bytes (54000
// otherwise; the error rolls back the call's writes).

pub fn stage_version(corpus: &str, request: &Value) -> ApiResult<String> {
    let col = load_collection(corpus, false)?;
    fit_response(
        stage_version_body(&col, corpus, request)?,
        col.config.limits.max_response_bytes,
    )
}

pub fn publish_version(corpus: &str, request: &Value) -> ApiResult<String> {
    let col = load_collection(corpus, false)?;
    fit_response(
        publish_version_body(&col, corpus, request)?,
        col.config.limits.max_response_bytes,
    )
}

pub fn retire(corpus: &str, request: &Value) -> ApiResult<String> {
    let col = load_collection(corpus, false)?;
    fit_response(
        retire_body(&col, corpus, request)?,
        col.config.limits.max_response_bytes,
    )
}

pub fn annotate(corpus: &str, request: &Value) -> ApiResult<String> {
    let col = load_collection(corpus, false)?;
    fit_response(
        annotate_body(&col, corpus, request)?,
        col.config.limits.max_response_bytes,
    )
}

pub fn purge(corpus: &str, request: &Value) -> ApiResult<String> {
    let col = load_collection(corpus, false)?;
    fit_response(
        purge_body(&col, corpus, request)?,
        col.config.limits.max_response_bytes,
    )
}
