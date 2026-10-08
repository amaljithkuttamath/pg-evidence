//! Read API. After loading collection_config, `query` and `resolve` each issue
//! exactly one retrieval statement, so a call observes one snapshot (G4).
//! Neither materializes a full source except `resolve`, bounded by
//! max_source_bytes, to verify the digest and span.

use crate::db::{load_collection, read_json, read_three_json, P};
use crate::error::{internal, precondition, ApiResult};
use crate::plan::{compile, parse_plan, status_case, Param, QueryContext, Tables};
use crate::render::{render_query, render_resolve, ResolveRow};
use serde_json::Value;

const EQ: &str = "OPERATOR(pg_catalog.=)";
const LE: &str = "OPERATOR(pg_catalog.<=)";

pub fn query(corpus: &str, request: &Value) -> ApiResult<String> {
    let col = load_collection(corpus, true)?;
    if col.statement_timeout == "0" {
        return Err(precondition(
            "statement_timeout_unset",
            "evidence.query requires a positive statement_timeout; it is the only bound on database work",
        ));
    }
    let limits = col.config.limits;
    let ctx = QueryContext {
        corpus,
        text_search_config: &col.config.text_search_config,
        vector_schema: col.vector_schema.as_deref(),
        embedding: col.config.embedding.as_ref(),
        limits: &limits,
    };
    let plan = parse_plan(request, &ctx)?;
    let compiled = compile(&plan, &ctx);
    let params = compiled
        .params
        .into_iter()
        .map(|p| match p {
            Param::Text(s) => P::Text(s),
            Param::TextArray(v) => P::TextArray(v),
            Param::UuidArray(v) => P::TextArray(v.iter().map(|u| u.to_string()).collect()),
            Param::Float4Array(v) => P::Float4Array(v),
        })
        .collect();
    let (results, edges, stats) = read_three_json(&compiled.sql, params)?;
    render_query(
        &plan,
        &results,
        &edges,
        &stats,
        col.semantic_readiness(),
        &mut || {
            pgrx::check_for_interrupts!();
        },
    )
}

fn parse_resolve_row(v: Value) -> ApiResult<ResolveRow> {
    let bad = || internal("unexpected resolve row");
    let a = v.as_array().filter(|a| a.len() == 15).ok_or_else(bad)?;
    let s = |i: usize| a[i].as_str().map(str::to_string).ok_or_else(bad);
    let opt_s = |i: usize| a[i].as_str().map(str::to_string);
    let n = |i: usize| a[i].as_i64().ok_or_else(bad);
    Ok(ResolveRow {
        evidence_id: s(0)?,
        version_id: s(1)?,
        asset_id: s(2)?,
        path: s(3)?,
        start_byte: n(4)? as i32,
        end_byte: n(5)? as i32,
        text: opt_s(6),
        status: s(7)?,
        source: opt_s(8),
        source_octets: a[9].as_i64().unwrap_or(0),
        source_sha256: s(10)?,
        byte_length: n(11)?,
        published_revision: a[12].as_i64(),
        tombstone: match (opt_s(13), opt_s(14)) {
            (Some(at), Some(reason)) => Some((at, reason)),
            _ => None,
        },
    })
}

/// Reads tombstone, version and span in one statement, so it cannot
/// interleave with a concurrent purge. The source is only fetched when it is
/// within max_source_bytes.
pub fn resolve(corpus: &str, evidence_id: &str) -> ApiResult<String> {
    let col = load_collection(corpus, true)?;
    let t = Tables::new(corpus);
    let max = col.config.limits.max_source_bytes;
    let sql = format!(
        "SELECT pg_catalog.json_build_array(s.evidence_id, s.version_id, v.asset_id, v.path, s.start_byte, \
         s.end_byte, s.text, {status}, \
         CASE WHEN pg_catalog.octet_length(v.source) {LE} $2 THEN v.source END, \
         pg_catalog.octet_length(v.source), pg_catalog.encode(v.source_sha256, 'hex'), v.byte_length, \
         p.revision, t.purged_at, t.reason) \
         FROM {spans} s {joins} WHERE s.evidence_id {EQ} $1::pg_catalog.uuid",
        status = status_case("v", "a", "p", "t"),
        spans = t.spans,
        joins = t.status_joins("s", "v", "a", "p", "t"),
    );
    let row = read_json(
        &sql,
        vec![P::Text(evidence_id.to_string()), P::Int4(max as i32)],
    )?;
    render_resolve(
        row.map(parse_resolve_row).transpose()?,
        max,
        col.config.limits.max_response_bytes,
    )
}
