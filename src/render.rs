//! JSON responses. Budgets are measured on the exact text returned: results
//! are dropped whole from the end, JSON is never cut, and if even the envelope
//! cannot fit the call fails with 54000.

use crate::error::{corrupted, internal, limit_exceeded, ApiError, ApiResult, SqlState};
use crate::model::{hex, sha256};
use crate::plan::{Op, Plan};
use serde_json::Value;
use std::collections::HashMap;

/// Writes a JSON object with keys in insertion order; values are escaped by serde_json.
pub struct JsonObject {
    buf: String,
}

impl Default for JsonObject {
    fn default() -> Self {
        Self::new()
    }
}

impl JsonObject {
    pub fn new() -> Self {
        JsonObject {
            buf: String::from("{"),
        }
    }

    fn key(&mut self, k: &str) {
        if self.buf.len() > 1 {
            self.buf.push(',');
        }
        self.buf.push_str(&Value::from(k).to_string());
        self.buf.push(':');
    }

    pub fn str(&mut self, k: &str, v: &str) -> &mut Self {
        self.key(k);
        self.buf.push_str(&Value::from(v).to_string());
        self
    }

    pub fn int(&mut self, k: &str, v: i64) -> &mut Self {
        self.key(k);
        self.buf.push_str(&v.to_string());
        self
    }

    pub fn opt_int(&mut self, k: &str, v: Option<i64>) -> &mut Self {
        match v {
            Some(n) => self.int(k, n),
            None => self.raw(k, "null"),
        }
    }

    pub fn bool(&mut self, k: &str, v: bool) -> &mut Self {
        self.raw(k, if v { "true" } else { "false" })
    }

    pub fn value(&mut self, k: &str, v: &Value) -> &mut Self {
        self.key(k);
        self.buf.push_str(&v.to_string());
        self
    }

    /// `json` must already be valid JSON text.
    pub fn raw(&mut self, k: &str, json: &str) -> &mut Self {
        self.key(k);
        self.buf.push_str(json);
        self
    }

    pub fn finish(mut self) -> String {
        self.buf.push('}');
        self.buf
    }
}

/// Longest prefix of `s` of at most `max` bytes ending on a character boundary.
pub fn trim_utf8(s: &str, max: usize) -> &str {
    if s.len() <= max {
        return s;
    }
    let mut i = max;
    while !s.is_char_boundary(i) {
        i -= 1;
    }
    &s[..i]
}

fn bad_row(what: &str) -> ApiError {
    internal(format!("query returned a malformed {what} row"))
}

struct RenderedResult {
    evidence_id: String,
    json: String,
    approximate: bool,
    excerpt_truncated: bool,
}

/// Result rows: [evidence_id, version_id, asset_id, path, start_byte, end_byte,
/// excerpt, mode, status, score] as produced by plan::compile.
fn render_result(v: &Value, excerpt_bytes: usize) -> ApiResult<RenderedResult> {
    let a = v
        .as_array()
        .filter(|a| a.len() == 10)
        .ok_or_else(|| bad_row("result"))?;
    let s = |i: usize| a[i].as_str().ok_or_else(|| bad_row("result"));
    let n = |i: usize| a[i].as_i64().ok_or_else(|| bad_row("result"));
    let (start, end, mode) = (n(4)?, n(5)?, s(7)?);
    let mut excerpt_truncated = false;
    let mut o = JsonObject::new();
    o.str("evidence_id", s(0)?)
        .str("version_id", s(1)?)
        .str("asset_id", s(2)?)
        .str("path", s(3)?)
        .int("start_byte", start)
        .int("end_byte", end)
        .str("status", s(8)?)
        .str("mode", mode);
    match a[6].as_str() {
        Some(text) => {
            let excerpt = trim_utf8(text, excerpt_bytes);
            excerpt_truncated = (excerpt.len() as i64) < end - start;
            o.str("excerpt", excerpt)
                .bool("excerpt_truncated", excerpt_truncated);
        }
        None => {
            o.raw("excerpt", "null").bool("excerpt_truncated", false);
        }
    }
    // Non-finite floats arrive as JSON strings ("NaN"); render them as null.
    let score = if a[9].is_number() {
        a[9].clone()
    } else {
        Value::Null
    };
    match mode {
        "lexical" => {
            o.value("rank", &score);
        }
        "semantic" => {
            o.value("distance", &score).bool("approximate", true);
        }
        _ => {}
    }
    Ok(RenderedResult {
        evidence_id: s(0)?.to_string(),
        json: o.finish(),
        approximate: mode == "semantic",
        excerpt_truncated,
    })
}

struct NodeStats {
    rows: i64,
    edges: i64,
}

/// Renders the response for `evidence.query` from the single statement's row.
/// `tick` is called in every loop (check_for_interrupts in the backend).
pub fn render_query(
    plan: &Plan,
    results: &Value,
    edges: &Value,
    stats: &Value,
    semantic_readiness: &str,
    tick: &mut dyn FnMut(),
) -> ApiResult<String> {
    let excerpt_bytes = plan.excerpt_bytes as usize;
    let empty = Vec::new();
    let result_rows = if results.is_null() {
        &empty
    } else {
        results.as_array().ok_or_else(|| bad_row("results"))?
    };
    let mut rendered = Vec::with_capacity(result_rows.len());
    let mut position: HashMap<String, usize> = HashMap::with_capacity(result_rows.len());
    for (i, r) in result_rows.iter().enumerate() {
        tick();
        let rr = render_result(r, excerpt_bytes)?;
        position.entry(rr.evidence_id.clone()).or_insert(i);
        rendered.push(rr);
    }

    // Edge rows: [node, source_id, kind, target_id, produced endpoint]. An edge
    // is rendered when the endpoint it produced is among the rendered results.
    let edge_rows = if edges.is_null() {
        &empty
    } else {
        edges.as_array().ok_or_else(|| bad_row("edges"))?
    };
    let mut edge_list: Vec<(usize, String)> = Vec::new();
    let mut edge_index: HashMap<(String, String, String), usize> = HashMap::new();
    for e in edge_rows {
        tick();
        let a = e
            .as_array()
            .filter(|a| a.len() == 5)
            .ok_or_else(|| bad_row("edge"))?;
        let s = |i: usize| a[i].as_str().ok_or_else(|| bad_row("edge"));
        let Some(&pos) = position.get(s(4)?) else {
            continue;
        };
        let key = (s(1)?.to_string(), s(2)?.to_string(), s(3)?.to_string());
        if let Some(&k) = edge_index.get(&key) {
            edge_list[k].0 = edge_list[k].0.min(pos);
            continue;
        }
        let mut o = JsonObject::new();
        o.str("source_evidence_id", &key.0)
            .str("kind", &key.1)
            .str("target_evidence_id", &key.2);
        edge_index.insert(key, edge_list.len());
        edge_list.push((pos, o.finish()));
    }

    let stat_rows = stats.as_array().ok_or_else(|| bad_row("stats"))?;
    if stat_rows.len() != plan.nodes.len() {
        return Err(bad_row("stats"));
    }
    let mut node_stats = Vec::with_capacity(stat_rows.len());
    for (i, st) in stat_rows.iter().enumerate() {
        let a = st
            .as_array()
            .filter(|a| a.len() == 3 && a[0].as_u64() == Some(i as u64))
            .ok_or_else(|| bad_row("stats"))?;
        node_stats.push(NodeStats {
            rows: a[1].as_i64().ok_or_else(|| bad_row("stats"))?,
            edges: a[2].as_i64().ok_or_else(|| bad_row("stats"))?,
        });
    }

    let mut nodes_json = String::from("[");
    let mut any_truncated = false;
    let mut edges_truncated = false;
    for (i, (node, st)) in plan.nodes.iter().zip(&node_stats).enumerate() {
        tick();
        let limit = node.limit as i64;
        let returned = st.rows.min(limit);
        let edge_overflow =
            matches!(node.op, Op::Neighbors { max_edges, .. } if st.edges > max_edges as i64);
        let truncated = st.rows > limit || edge_overflow;
        any_truncated |= truncated;
        edges_truncated |= edge_overflow;
        let mut o = JsonObject::new();
        o.str("id", &node.id)
            .str("op", node.op.name())
            .int("requested", limit)
            .int("returned", returned)
            .bool("truncated", truncated)
            .bool("underfilled", returned < limit);
        if i > 0 {
            nodes_json.push(',');
        }
        nodes_json.push_str(&o.finish());
    }
    nodes_json.push(']');

    let readiness = {
        let mut o = JsonObject::new();
        o.str("literal", "ready")
            .str("regex", "ready")
            .str("lexical", "ready")
            .str("semantic", semantic_readiness);
        o.finish()
    };
    let output_limit = plan.nodes[plan.output].limit as i64;
    let output_truncated = node_stats[plan.output].rows > output_limit;
    let approximate = plan.approximate() || rendered.iter().any(|r| r.approximate);

    let mut build = |k: usize| -> String {
        let mut out = String::from("{\"results\":[");
        for (i, r) in rendered[..k].iter().enumerate() {
            tick();
            if i > 0 {
                out.push(',');
            }
            out.push_str(&r.json);
        }
        out.push_str("],\"edges\":[");
        let mut first = true;
        for (pos, json) in &edge_list {
            tick();
            if *pos < k {
                if !first {
                    out.push(',');
                }
                first = false;
                out.push_str(json);
            }
        }
        out.push_str("],");
        let dropped = rendered.len() - k;
        // A cut excerpt is truncated content: the response is then not complete.
        let excerpts_truncated = rendered[..k].iter().any(|r| r.excerpt_truncated);
        let mut truncation = JsonObject::new();
        truncation
            .int("requested", output_limit)
            .int("returned", k as i64)
            .bool("truncated", output_truncated || dropped > 0)
            .bool("underfilled", (k as i64) < output_limit)
            .int("dropped_for_budget", dropped as i64)
            .bool("edges_truncated", edges_truncated)
            .bool("excerpts_truncated", excerpts_truncated);
        let mut rest = JsonObject::new();
        rest.raw("truncation", &truncation.finish())
            .raw("nodes", &nodes_json)
            .raw("readiness", &readiness)
            .bool("approximate", approximate)
            .bool(
                "complete",
                !any_truncated && dropped == 0 && !approximate && !excerpts_truncated,
            );
        // Splice the remaining keys into the object opened above.
        out.push_str(&rest.finish()[1..]);
        out
    };

    let budget = plan.max_response_bytes as usize;
    let n = rendered.len();
    let full = build(n);
    if full.len() <= budget {
        return Ok(full);
    }
    let envelope = build(0);
    if envelope.len() > budget {
        return Err(ApiError::new(
            SqlState::ProgramLimit,
            "response_envelope_too_large",
            format!(
                "the response envelope needs {} bytes; max_response_bytes is {budget}",
                envelope.len()
            ),
        )
        .with("max", budget as u64));
    }
    // Largest k whose rendering fits; build(lo) always fits.
    let (mut lo, mut hi, mut best) = (0usize, n, envelope);
    while hi - lo > 1 {
        let mid = lo + (hi - lo) / 2;
        let candidate = build(mid);
        if candidate.len() <= budget {
            lo = mid;
            best = candidate;
        } else {
            hi = mid;
        }
    }
    Ok(best)
}

/// One row of the resolve statement (db layer).
#[derive(Debug, Clone)]
pub struct ResolveRow {
    pub evidence_id: String,
    pub version_id: String,
    pub asset_id: String,
    pub path: String,
    pub start_byte: i32,
    pub end_byte: i32,
    pub text: Option<String>,
    pub status: String,
    /// NULL when purged, or when larger than max_source_bytes (not fetched).
    pub source: Option<String>,
    pub source_octets: i64,
    pub source_sha256: String,
    pub byte_length: i64,
    pub published_revision: Option<i64>,
    /// (purged_at, reason)
    pub tombstone: Option<(String, String)>,
}

/// Fails with 54000 when a response that cannot be shortened (resolve and
/// mutation results) exceeds max_response_bytes. Writes made by the call roll
/// back with the error.
pub fn fit_response(response: String, max_response_bytes: u32) -> ApiResult<String> {
    if response.len() > max_response_bytes as usize {
        return Err(ApiError::new(
            SqlState::ProgramLimit,
            "response_too_large",
            format!(
                "the response needs {} bytes; max_response_bytes is {max_response_bytes}",
                response.len()
            ),
        )
        .with("max", max_response_bytes as u64)
        .with("bytes", response.len() as u64));
    }
    Ok(response)
}

/// Verifies retained bytes and renders `evidence.resolve`. The exact span text
/// is never cut: a response over max_response_bytes is 54000.
pub fn render_resolve(
    row: Option<ResolveRow>,
    max_source_bytes: u32,
    max_response_bytes: u32,
) -> ApiResult<String> {
    let Some(r) = row else {
        return Ok("{\"status\":\"not_found\"}".to_string());
    };
    let mut o = JsonObject::new();
    o.str(
        "status",
        if r.tombstone.is_some() {
            "purged"
        } else {
            &r.status
        },
    )
    .str("evidence_id", &r.evidence_id)
    .str("version_id", &r.version_id)
    .str("asset_id", &r.asset_id)
    .str("path", &r.path)
    .int("start_byte", r.start_byte as i64)
    .int("end_byte", r.end_byte as i64)
    .str("source_sha256", &r.source_sha256)
    .int("byte_length", r.byte_length)
    .opt_int("published_revision", r.published_revision);
    if let Some((purged_at, reason)) = &r.tombstone {
        o.str("purged_at", purged_at).str("reason", reason);
        return fit_response(o.finish(), max_response_bytes);
    }
    if r.source_octets > max_source_bytes as i64 {
        return Err(limit_exceeded(
            "max_source_bytes",
            max_source_bytes as u64,
            "the retained source exceeds max_source_bytes and cannot be verified",
        ));
    }
    let source = r.source.as_deref().ok_or_else(|| {
        corrupted(
            "source_missing",
            "retained source is missing without a purge tombstone",
        )
    })?;
    if source.len() as i64 != r.byte_length || hex(&sha256(source.as_bytes())) != r.source_sha256 {
        return Err(corrupted(
            "digest_mismatch",
            "retained source does not match its SHA-256 digest",
        )
        .with("version_id", r.version_id.as_str()));
    }
    let text = r.text.as_deref().ok_or_else(|| {
        corrupted(
            "span_missing",
            "span text is missing without a purge tombstone",
        )
    })?;
    let (s, e) = (r.start_byte as usize, r.end_byte as usize);
    let matches = r.start_byte >= 0
        && s < e
        && e <= source.len()
        && source.is_char_boundary(s)
        && source.is_char_boundary(e)
        && &source[s..e] == text;
    if !matches {
        return Err(
            corrupted("span_mismatch", "span text does not match its source slice")
                .with("evidence_id", r.evidence_id.as_str()),
        );
    }
    o.str("text", text).bool("verified", true);
    fit_response(o.finish(), max_response_bytes)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::error::SqlState;
    use crate::model::{hex, sha256, Limits};
    use crate::plan::{parse_plan, QueryContext};
    use serde_json::json;

    const E1: &str = "00000000-0000-4000-8000-000000000001";
    const E2: &str = "00000000-0000-4000-8000-000000000002";
    const E3: &str = "00000000-0000-4000-8000-000000000003";
    const V: &str = "00000000-0000-4000-8000-0000000000aa";
    const A: &str = "00000000-0000-4000-8000-0000000000bb";

    fn plan(v: Value) -> Plan {
        let limits = Limits::DEFAULT;
        let ctx = QueryContext {
            corpus: "docs",
            text_search_config: "pg_catalog.simple",
            vector_schema: None,
            embedding: None,
            limits: &limits,
        };
        parse_plan(&v, &ctx).unwrap()
    }

    fn row(e: &str, start: i64, text: &str, mode: &str, score: Value) -> Value {
        json!([
            e,
            V,
            A,
            "docs/a.md",
            start,
            start + text.len() as i64,
            text,
            mode,
            "current",
            score
        ])
    }

    fn render(p: &Plan, results: Value, edges: Value, stats: Value) -> ApiResult<String> {
        render_query(p, &results, &edges, &stats, "not_configured", &mut || {})
    }

    #[test]
    fn renders_valid_json_within_budget() {
        let p = plan(
            json!({"nodes": [{"id": "a", "op": "lexical", "query": "x", "limit": 3}], "output": "a"}),
        );
        let out = render(
            &p,
            json!([
                row(E1, 0, "alpha \"quoted\" é", "lexical", json!(0.5)),
                row(E2, 20, "beta", "lexical", json!(0.25))
            ]),
            Value::Null,
            json!([[0, 2, 0]]),
        )
        .unwrap();
        let v: Value = serde_json::from_str(&out).unwrap();
        assert_eq!(v["results"].as_array().unwrap().len(), 2);
        assert_eq!(v["results"][0]["excerpt"], "alpha \"quoted\" é");
        assert_eq!(v["results"][0]["rank"], 0.5);
        assert_eq!(v["results"][0]["excerpt_truncated"], false);
        assert_eq!(
            v["truncation"],
            json!({"requested": 3, "returned": 2, "truncated": false,
            "underfilled": true, "dropped_for_budget": 0, "edges_truncated": false, "excerpts_truncated": false})
        );
        assert_eq!(
            v["nodes"][0],
            json!({"id": "a", "op": "lexical", "requested": 3, "returned": 2, "truncated": false, "underfilled": true})
        );
        assert_eq!(
            v["readiness"],
            json!({"literal": "ready", "regex": "ready", "lexical": "ready", "semantic": "not_configured"})
        );
        assert_eq!(v["complete"], true);
        assert_eq!(v["approximate"], false);
    }

    #[test]
    fn drops_whole_results_from_the_end_to_fit_exact_budget() {
        let rows: Vec<Value> = (0..5)
            .map(|i| {
                row(
                    &format!("00000000-0000-4000-8000-00000000000{i}"),
                    i * 10,
                    "some evidence text",
                    "literal",
                    Value::Null,
                )
            })
            .collect();
        let full = {
            let p = plan(
                json!({"nodes": [{"id": "a", "op": "literal", "text": "x", "limit": 5}], "output": "a"}),
            );
            render(&p, json!(rows), Value::Null, json!([[0, 5, 0]])).unwrap()
        };
        // Budget exactly equal to the full response keeps everything.
        let p = plan(
            json!({"nodes": [{"id": "a", "op": "literal", "text": "x", "limit": 5}], "output": "a",
                            "max_response_bytes": full.len()}),
        );
        assert_eq!(
            render(&p, json!(rows), Value::Null, json!([[0, 5, 0]])).unwrap(),
            full
        );
        // One byte less must drop at least one whole result and stay within budget.
        for budget in [full.len() - 1, full.len() / 2, 600] {
            let p = plan(
                json!({"nodes": [{"id": "a", "op": "literal", "text": "x", "limit": 5}], "output": "a",
                                "max_response_bytes": budget, "excerpt_bytes": 100}),
            );
            let out = render(&p, json!(rows), Value::Null, json!([[0, 5, 0]])).unwrap();
            assert!(out.len() <= budget, "{} > {budget}", out.len());
            let v: Value = serde_json::from_str(&out).unwrap();
            let n = v["results"].as_array().unwrap().len();
            assert!(n < 5);
            assert_eq!(v["truncation"]["returned"], n);
            assert_eq!(v["truncation"]["dropped_for_budget"], 5 - n);
            assert_eq!(v["truncation"]["truncated"], true);
            assert_eq!(v["complete"], false);
            // Kept results are a prefix, in order.
            for (i, r) in v["results"].as_array().unwrap().iter().enumerate() {
                assert_eq!(r["start_byte"], i * 10);
            }
        }
        // The envelope alone cannot fit: 54000.
        let p = plan(
            json!({"nodes": [{"id": "a", "op": "literal", "text": "x", "limit": 5}], "output": "a",
                            "max_response_bytes": 50, "excerpt_bytes": 10}),
        );
        let e = render(&p, json!(rows), Value::Null, json!([[0, 5, 0]])).unwrap_err();
        assert_eq!(
            (e.state, e.reason()),
            (SqlState::ProgramLimit, "response_envelope_too_large")
        );
    }

    #[test]
    fn excerpts_are_cut_on_character_boundaries() {
        let p = plan(
            json!({"nodes": [{"id": "a", "op": "literal", "text": "x"}], "output": "a", "excerpt_bytes": 5}),
        );
        // SQL returns up to excerpt_bytes characters; rendering trims to bytes.
        let out = render(
            &p,
            json!([row(E1, 0, "ééééé", "literal", Value::Null)]),
            Value::Null,
            json!([[0, 1, 0]]),
        )
        .unwrap();
        let v: Value = serde_json::from_str(&out).unwrap();
        assert_eq!(v["results"][0]["excerpt"], "éé");
        assert_eq!(v["results"][0]["excerpt_truncated"], true);
        assert_eq!(v["truncation"]["excerpts_truncated"], true);
        assert_eq!(
            v["complete"], false,
            "a cut excerpt is never labelled complete"
        );
        assert_eq!(trim_utf8("a😀", 4), "a");
        assert_eq!(trim_utf8("a😀", 5), "a😀");
    }

    #[test]
    fn truncation_and_edges() {
        let p = plan(json!({"nodes": [
            {"id": "a", "op": "literal", "text": "x", "limit": 1},
            {"id": "n", "op": "neighbors", "from": "a", "limit": 1, "max_edges": 2},
            {"id": "u", "op": "union", "inputs": ["a", "n"]}
        ], "output": "u"}));
        let purged = json!([
            E3,
            V,
            A,
            "docs/a.md",
            0,
            4,
            null,
            "neighbors",
            "purged",
            null
        ]);
        let out = render(
            &p,
            json!([row(E1, 0, "abcd", "literal", Value::Null), purged]),
            json!([[1, E1, "cites", E3, E3], [1, E2, "cites", E1, E2]]),
            json!([[0, 2, 0], [1, 2, 3], [2, 2, 0]]),
        )
        .unwrap();
        let v: Value = serde_json::from_str(&out).unwrap();
        assert_eq!(v["results"][1]["status"], "purged");
        assert_eq!(v["results"][1]["excerpt"], Value::Null);
        // Only edges whose produced endpoint was rendered.
        assert_eq!(
            v["edges"],
            json!([{"source_evidence_id": E1, "kind": "cites", "target_evidence_id": E3}])
        );
        assert_eq!(v["nodes"][0]["truncated"], true); // 2 > limit 1
        assert_eq!(v["nodes"][1]["truncated"], true); // 3 edges > max_edges 2
        assert_eq!(v["truncation"]["edges_truncated"], true);
        assert_eq!(v["complete"], false);
    }

    #[test]
    fn semantic_rows_are_marked_approximate() {
        let p = plan(json!({"nodes": [{"id": "a", "op": "literal", "text": "x"}], "output": "a"}));
        let out = render(
            &p,
            json!([
                row(E1, 0, "abc", "semantic", json!(0.125)),
                row(E2, 0, "abc", "semantic", json!("NaN"))
            ]),
            Value::Null,
            json!([[0, 2, 0]]),
        )
        .unwrap();
        let v: Value = serde_json::from_str(&out).unwrap();
        assert_eq!(v["results"][0]["distance"], 0.125);
        assert_eq!(v["results"][0]["approximate"], true);
        assert_eq!(v["results"][1]["distance"], Value::Null);
        assert_eq!(v["complete"], false);
    }

    #[test]
    fn malformed_sql_rows_are_internal_errors_not_panics() {
        let p = plan(json!({"nodes": [{"id": "a", "op": "literal", "text": "x"}], "output": "a"}));
        assert!(render(&p, json!([[1, 2]]), Value::Null, json!([[0, 1, 0]])).is_err());
    }

    fn resolve_row(source: &str, start: i32, end: i32) -> ResolveRow {
        ResolveRow {
            evidence_id: E1.into(),
            version_id: V.into(),
            asset_id: A.into(),
            path: "a.md".into(),
            start_byte: start,
            end_byte: end,
            text: Some(source[start as usize..end as usize].to_string()),
            status: "current".into(),
            source: Some(source.to_string()),
            source_octets: source.len() as i64,
            source_sha256: hex(&sha256(source.as_bytes())),
            byte_length: source.len() as i64,
            published_revision: Some(1),
            tombstone: None,
        }
    }

    #[test]
    fn resolve_verifies_bytes() {
        let src = "héllo\r\nwörld";
        let ok = render_resolve(Some(resolve_row(src, 8, 14)), 1 << 20, 1 << 20).unwrap();
        let v: Value = serde_json::from_str(&ok).unwrap();
        assert_eq!(v["text"], "wörld");
        assert_eq!(v["status"], "current");
        assert_eq!(v["verified"], true);

        assert_eq!(
            render_resolve(None, 1 << 20, 1 << 20).unwrap(),
            "{\"status\":\"not_found\"}"
        );

        let mut bad = resolve_row(src, 8, 14);
        bad.text = Some("world".into());
        assert_eq!(
            render_resolve(Some(bad), 1 << 20, 1 << 20)
                .unwrap_err()
                .state,
            SqlState::DataCorrupted
        );

        let mut bad = resolve_row(src, 8, 14);
        bad.source = Some(src.replace('h', "H"));
        assert_eq!(
            render_resolve(Some(bad), 1 << 20, 1 << 20)
                .unwrap_err()
                .state,
            SqlState::DataCorrupted
        );

        // Offsets that no longer fall on character boundaries are corruption, not a panic.
        let mut bad = resolve_row(src, 8, 14);
        bad.start_byte = 2;
        assert_eq!(
            render_resolve(Some(bad), 1 << 20, 1 << 20)
                .unwrap_err()
                .state,
            SqlState::DataCorrupted
        );
        let mut bad = resolve_row(src, 8, 14);
        bad.end_byte = 99;
        assert_eq!(
            render_resolve(Some(bad), 1 << 20, 1 << 20)
                .unwrap_err()
                .state,
            SqlState::DataCorrupted
        );

        let mut missing = resolve_row(src, 8, 14);
        missing.source = None;
        assert_eq!(
            render_resolve(Some(missing), 1 << 20, 1 << 20)
                .unwrap_err()
                .state,
            SqlState::DataCorrupted
        );

        // The exact text is never cut: an over-budget resolve is 54000.
        let e = render_resolve(Some(resolve_row(src, 8, 14)), 1 << 20, 40).unwrap_err();
        assert_eq!(
            (e.state, e.reason()),
            (SqlState::ProgramLimit, "response_too_large")
        );

        let big = resolve_row(src, 8, 14);
        assert_eq!(
            render_resolve(Some(big), 4, 1 << 20).unwrap_err().state,
            SqlState::ProgramLimit
        );

        let mut purged = resolve_row(src, 8, 14);
        purged.source = None;
        purged.text = None;
        purged.status = "purged".into();
        purged.tombstone = Some(("2026-10-07T00:00:00Z".into(), "gdpr".into()));
        let v: Value =
            serde_json::from_str(&render_resolve(Some(purged), 1 << 20, 1 << 20).unwrap()).unwrap();
        assert_eq!(v["status"], "purged");
        assert_eq!(v["reason"], "gdpr");
        assert!(v.get("text").is_none());
    }

    #[test]
    fn json_object_writer_escapes() {
        let mut o = JsonObject::new();
        o.str("a", "x\"\n\u{0001}");
        o.int("b", -3);
        o.raw("c", "[1]");
        assert_eq!(o.finish(), "{\"a\":\"x\\\"\\n\\u0001\",\"b\":-3,\"c\":[1]}");
    }
}
