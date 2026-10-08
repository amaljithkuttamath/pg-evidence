//! `evidence.query` plans: validation of the bounded DAG request and its
//! compilation into ONE SQL statement, so a call observes one snapshot (G4).
//! Request values are always bound as parameters. Only quoted identifiers,
//! the collection's quoted text-search configuration and validated integers
//! are interpolated. Functions and operators are schema-qualified.

use crate::error::{invalid, limit_exceeded, precondition, ApiError, ApiResult, SqlState};
use crate::model::{
    array, boolean, is_simple_name, object, optional, relation_kind, required, string, tag, text,
    uint, vector, Embedding, Limits, Uuid16, MAX_PATH_BYTES,
};
use serde_json::Value;

pub const DEFAULT_LIMIT: u32 = 10;
pub const DEFAULT_EXCERPT_BYTES: u32 = 512;
/// Literal text, regex patterns and lexical queries.
pub const MAX_PATTERN_BYTES: usize = 1024;
pub const MAX_FILTER_TAGS: usize = 64;
pub const MAX_FILTER_ASSETS: usize = 1000;
pub const MAX_UNION_INPUTS: usize = 16;
/// Protective cap on the excerpt payload the evidence statement can build
/// before rendering: 6 bytes per SQL character (left() counts characters; JSON
/// escaping expands) x output limit x excerpt_bytes. Not a memory/RSS limit.
pub const MAX_EXCERPT_MATERIALIZATION_BYTES: u64 = 64 << 20;
pub const EXCERPT_BYTES_PER_CHAR_BOUND: u64 = 6;
pub const STATUSES: [&str; 5] = ["current", "historical", "staged", "retired", "purged"];

const EQ: &str = "OPERATOR(pg_catalog.=)";
const LE: &str = "OPERATOR(pg_catalog.<=)";
const GT: &str = "OPERATOR(pg_catalog.>)";

/// Always double-quotes, so reserved words and any characters are safe.
pub fn quote_ident(s: &str) -> String {
    format!("\"{}\"", s.replace('"', "\"\""))
}

/// Same output as PostgreSQL's quote_literal.
pub fn quote_literal(s: &str) -> String {
    let body = s.replace('\'', "''");
    if s.contains('\\') {
        format!("E'{}'", body.replace('\\', "\\\\"))
    } else {
        format!("'{body}'")
    }
}

pub struct QueryContext<'a> {
    pub corpus: &'a str,
    /// Schema-qualified, identifier-quoted regconfig name from collection_config.
    pub text_search_config: &'a str,
    /// pgvector's schema read from pg_extension at call time.
    pub vector_schema: Option<&'a str>,
    pub embedding: Option<&'a Embedding>,
    pub limits: &'a Limits,
}

#[derive(Debug, Clone, PartialEq)]
pub enum Param {
    Text(String),
    TextArray(Vec<String>),
    UuidArray(Vec<Uuid16>),
    Float4Array(Vec<f32>),
}

#[derive(Debug)]
pub struct Compiled {
    pub sql: String,
    pub params: Vec<Param>,
}

#[derive(Debug, Clone, Default)]
pub struct Filter {
    pub path_prefix: Option<String>,
    pub tags_all: Vec<String>,
    pub tags_any: Vec<String>,
    pub asset_ids: Vec<Uuid16>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Syntax {
    Websearch,
    Plain,
    Phrase,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Direction {
    Out,
    In,
    Both,
}

#[derive(Debug, Clone)]
pub enum Op {
    Literal {
        text: String,
        filter: Filter,
    },
    Regex {
        pattern: String,
        case_insensitive: bool,
        filter: Filter,
    },
    Lexical {
        query: String,
        syntax: Syntax,
        filter: Filter,
    },
    Semantic {
        vector: Vec<f32>,
        filter: Filter,
    },
    Neighbors {
        from: usize,
        direction: Direction,
        kinds: Vec<String>,
        statuses: Vec<String>,
        max_edges: u32,
    },
    Union {
        inputs: Vec<usize>,
    },
}

impl Op {
    pub fn name(&self) -> &'static str {
        match self {
            Op::Literal { .. } => "literal",
            Op::Regex { .. } => "regex",
            Op::Lexical { .. } => "lexical",
            Op::Semantic { .. } => "semantic",
            Op::Neighbors { .. } => "neighbors",
            Op::Union { .. } => "union",
        }
    }

    fn inputs(&self) -> Vec<usize> {
        match self {
            Op::Neighbors { from, .. } => vec![*from],
            Op::Union { inputs } => inputs.clone(),
            _ => vec![],
        }
    }
}

#[derive(Debug, Clone)]
pub struct Node {
    pub id: String,
    pub op: Op,
    pub limit: u32,
}

#[derive(Debug, Clone)]
pub struct Plan {
    pub nodes: Vec<Node>,
    pub output: usize,
    pub excerpt_bytes: u32,
    pub max_response_bytes: u32,
}

impl Plan {
    /// Semantic retrieval uses an approximate index; every node feeds the output.
    pub fn approximate(&self) -> bool {
        self.nodes
            .iter()
            .any(|n| matches!(n.op, Op::Semantic { .. }))
    }
}

// ---------------------------------------------------------------------------
// Validation

fn pattern(m: &serde_json::Map<String, Value>, p: &str, key: &str) -> ApiResult<String> {
    let path = format!("{p}.{key}");
    let s = string(required(m, p, key)?, &path)?;
    if s.is_empty() || s.len() > MAX_PATTERN_BYTES {
        return Err(invalid(
            &path,
            format!("{path} must be 1 to {MAX_PATTERN_BYTES} bytes"),
        ));
    }
    Ok(s.to_string())
}

fn string_list<T>(
    v: &Value,
    path: &str,
    max: usize,
    item: impl Fn(&Value, &str) -> ApiResult<T>,
) -> ApiResult<Vec<T>> {
    let items = array(v, path)?;
    if items.len() > max {
        return Err(invalid(path, format!("{path} allows at most {max} items")));
    }
    items
        .iter()
        .enumerate()
        .map(|(i, x)| item(x, &format!("{path}[{i}]")))
        .collect()
}

fn parse_filter(m: &serde_json::Map<String, Value>, p: &str) -> ApiResult<Filter> {
    let Some(fv) = optional(m, "filter") else {
        return Ok(Filter::default());
    };
    let fp = format!("{p}.filter");
    let fm = object(
        fv,
        &fp,
        &["path_prefix", "tags_all", "tags_any", "asset_ids"],
    )?;
    Ok(Filter {
        path_prefix: optional(fm, "path_prefix")
            .map(|v| text(v, &format!("{fp}.path_prefix"), MAX_PATH_BYTES))
            .transpose()?,
        tags_all: optional(fm, "tags_all")
            .map(|v| string_list(v, &format!("{fp}.tags_all"), MAX_FILTER_TAGS, tag))
            .transpose()?
            .unwrap_or_default(),
        tags_any: optional(fm, "tags_any")
            .map(|v| string_list(v, &format!("{fp}.tags_any"), MAX_FILTER_TAGS, tag))
            .transpose()?
            .unwrap_or_default(),
        asset_ids: optional(fm, "asset_ids")
            .map(|v| {
                string_list(
                    v,
                    &format!("{fp}.asset_ids"),
                    MAX_FILTER_ASSETS,
                    Uuid16::from_value,
                )
            })
            .transpose()?
            .unwrap_or_default(),
    })
}

fn parse_node(v: &Value, index: usize, earlier: &[Node], ctx: &QueryContext) -> ApiResult<Node> {
    let p = format!("nodes[{index}]");
    let op_name = v.get("op").and_then(Value::as_str).unwrap_or("");
    let allowed: &[&str] = match op_name {
        "literal" => &["id", "op", "text", "limit", "filter"],
        "regex" => &["id", "op", "pattern", "case_insensitive", "limit", "filter"],
        "lexical" => &["id", "op", "query", "syntax", "limit", "filter"],
        "semantic" => &["id", "op", "model", "vector", "limit", "filter"],
        "neighbors" => &[
            "id",
            "op",
            "from",
            "direction",
            "kinds",
            "status",
            "limit",
            "max_edges",
        ],
        "union" => &["id", "op", "inputs", "limit"],
        _ => {
            return Err(invalid(
                &format!("{p}.op"),
                "op must be one of literal, regex, lexical, semantic, neighbors, union",
            ))
        }
    };
    let m = object(v, &p, allowed)?;
    let id = string(required(m, &p, "id")?, &format!("{p}.id"))?;
    if !is_simple_name(id, 32) {
        return Err(invalid(
            &format!("{p}.id"),
            "node id must match ^[a-z][a-z0-9_]{0,31}$",
        ));
    }
    if earlier.iter().any(|n| n.id == id) {
        return Err(invalid(
            &format!("{p}.id"),
            format!("duplicate node id \"{id}\""),
        ));
    }
    let earlier_node = |v: &Value, path: &str| -> ApiResult<usize> {
        let name = string(v, path)?;
        earlier.iter().position(|n| n.id == name).ok_or_else(|| {
            invalid(
                path,
                format!("{path} must name an earlier node, not \"{name}\""),
            )
        })
    };
    let max_candidates = ctx.limits.max_candidates_per_operator;
    let explicit_limit = match optional(m, "limit") {
        None => None,
        Some(lv) => {
            let path = format!("{p}.limit");
            let n = uint(lv, &path, u32::MAX as u64)? as u32;
            if n == 0 {
                return Err(invalid(&path, format!("{path} must be at least 1")));
            }
            if n > max_candidates {
                return Err(limit_exceeded(
                    "max_candidates_per_operator",
                    max_candidates as u64,
                    format!("{path} exceeds max_candidates_per_operator"),
                ));
            }
            Some(n)
        }
    };

    let op = match op_name {
        "literal" => Op::Literal {
            text: pattern(m, &p, "text")?,
            filter: parse_filter(m, &p)?,
        },
        "regex" => Op::Regex {
            pattern: pattern(m, &p, "pattern")?,
            case_insensitive: optional(m, "case_insensitive")
                .map(|v| boolean(v, &format!("{p}.case_insensitive")))
                .transpose()?
                .unwrap_or(false),
            filter: parse_filter(m, &p)?,
        },
        "lexical" => Op::Lexical {
            query: pattern(m, &p, "query")?,
            syntax: match optional(m, "syntax")
                .map(|v| string(v, &format!("{p}.syntax")))
                .transpose()?
            {
                None | Some("websearch") => Syntax::Websearch,
                Some("plain") => Syntax::Plain,
                Some("phrase") => Syntax::Phrase,
                Some(_) => {
                    return Err(invalid(
                        &format!("{p}.syntax"),
                        "syntax must be websearch, plain or phrase",
                    ))
                }
            },
            filter: parse_filter(m, &p)?,
        },
        "semantic" => {
            let Some(emb) = ctx.embedding else {
                return Err(ApiError::new(
                    SqlState::InvalidParameter,
                    "semantic_not_configured",
                    "this collection has no embedding configuration",
                )
                .with("field", format!("{p}.op")));
            };
            if ctx.vector_schema.is_none() {
                return Err(precondition(
                    "pgvector_missing",
                    "the vector extension is not installed",
                ));
            }
            let model = string(required(m, &p, "model")?, &format!("{p}.model"))?;
            if model != emb.model {
                return Err(invalid(
                    &format!("{p}.model"),
                    format!(
                        "model must be the collection's embedding_model \"{}\"",
                        emb.model
                    ),
                ));
            }
            Op::Semantic {
                vector: vector(
                    required(m, &p, "vector")?,
                    &format!("{p}.vector"),
                    emb.dimensions,
                )?,
                filter: parse_filter(m, &p)?,
            }
        }
        "neighbors" => Op::Neighbors {
            from: earlier_node(required(m, &p, "from")?, &format!("{p}.from"))?,
            direction: match optional(m, "direction")
                .map(|v| string(v, &format!("{p}.direction")))
                .transpose()?
            {
                None | Some("both") => Direction::Both,
                Some("out") => Direction::Out,
                Some("in") => Direction::In,
                Some(_) => {
                    return Err(invalid(
                        &format!("{p}.direction"),
                        "direction must be out, in or both",
                    ))
                }
            },
            kinds: optional(m, "kinds")
                .map(|v| string_list(v, &format!("{p}.kinds"), MAX_FILTER_TAGS, relation_kind))
                .transpose()?
                .unwrap_or_default(),
            statuses: match optional(m, "status") {
                None => vec![],
                Some(sv) => string_list(sv, &format!("{p}.status"), STATUSES.len(), |x, path| {
                    let s = string(x, path)?;
                    if STATUSES.contains(&s) {
                        Ok(s.to_string())
                    } else {
                        Err(invalid(
                            path,
                            format!("{path} must be one of {}", STATUSES.join(", ")),
                        ))
                    }
                })?,
            },
            // 0 = take a share of max_edges_returned; resolved in parse_plan.
            max_edges: match optional(m, "max_edges") {
                None => 0,
                Some(x) => {
                    let path = format!("{p}.max_edges");
                    let n = uint(x, &path, u32::MAX as u64)? as u32;
                    if n == 0 {
                        return Err(invalid(&path, format!("{path} must be at least 1")));
                    }
                    n
                }
            },
        },
        "union" => {
            let ip = format!("{p}.inputs");
            let items = array(required(m, &p, "inputs")?, &ip)?;
            if items.len() < 2 || items.len() > MAX_UNION_INPUTS {
                return Err(invalid(
                    &ip,
                    format!("{ip} must name 2 to {MAX_UNION_INPUTS} nodes"),
                ));
            }
            let mut inputs = Vec::with_capacity(items.len());
            for (i, x) in items.iter().enumerate() {
                let n = earlier_node(x, &format!("{ip}[{i}]"))?;
                if inputs.contains(&n) {
                    return Err(invalid(
                        &format!("{ip}[{i}]"),
                        "union inputs must be distinct",
                    ));
                }
                inputs.push(n);
            }
            Op::Union { inputs }
        }
        _ => unreachable!("op validated above"),
    };
    let limit = match (&op, explicit_limit) {
        (_, Some(n)) => n,
        (Op::Union { inputs }, None) => {
            let sum: u64 = inputs.iter().map(|i| earlier[*i].limit as u64).sum();
            sum.min(max_candidates as u64) as u32
        }
        _ => DEFAULT_LIMIT.min(max_candidates),
    };
    Ok(Node {
        id: id.to_string(),
        op,
        limit,
    })
}

pub fn parse_plan(v: &Value, ctx: &QueryContext) -> ApiResult<Plan> {
    let limits = ctx.limits;
    let m = object(
        v,
        "",
        &["nodes", "output", "excerpt_bytes", "max_response_bytes"],
    )?;
    let node_values = array(required(m, "", "nodes")?, "nodes")?;
    if node_values.is_empty() {
        return Err(invalid("nodes", "nodes must not be empty"));
    }
    if node_values.len() > limits.max_plan_nodes as usize {
        return Err(limit_exceeded(
            "max_plan_nodes",
            limits.max_plan_nodes as u64,
            format!("plan has {} nodes", node_values.len()),
        ));
    }
    let max_response_bytes = match optional(m, "max_response_bytes") {
        None => limits.max_response_bytes,
        Some(x) => {
            let n = uint(x, "max_response_bytes", u32::MAX as u64)? as u32;
            if n == 0 {
                return Err(invalid(
                    "max_response_bytes",
                    "max_response_bytes must be at least 1",
                ));
            }
            if n > limits.max_response_bytes {
                return Err(limit_exceeded(
                    "max_response_bytes",
                    limits.max_response_bytes as u64,
                    "max_response_bytes exceeds the collection limit",
                ));
            }
            n
        }
    };
    let excerpt_bytes = match optional(m, "excerpt_bytes") {
        None => DEFAULT_EXCERPT_BYTES.min(max_response_bytes),
        Some(x) => {
            let n = uint(x, "excerpt_bytes", u32::MAX as u64)? as u32;
            if n > max_response_bytes {
                return Err(limit_exceeded(
                    "max_response_bytes",
                    max_response_bytes as u64,
                    "excerpt_bytes exceeds max_response_bytes",
                ));
            }
            n
        }
    };

    let mut nodes: Vec<Node> = Vec::with_capacity(node_values.len());
    for (i, nv) in node_values.iter().enumerate() {
        let node = parse_node(nv, i, &nodes, ctx)?;
        nodes.push(node);
    }
    let output_name = string(required(m, "", "output")?, "output")?;
    let output = nodes
        .iter()
        .position(|n| n.id == output_name)
        .ok_or_else(|| invalid("output", format!("output names no node: \"{output_name}\"")))?;

    let materialization =
        EXCERPT_BYTES_PER_CHAR_BOUND * nodes[output].limit as u64 * excerpt_bytes as u64;
    if materialization > MAX_EXCERPT_MATERIALIZATION_BYTES {
        return Err(limit_exceeded(
            "max_excerpt_materialization_bytes",
            MAX_EXCERPT_MATERIALIZATION_BYTES,
            format!(
                "6 x output limit x excerpt_bytes is {materialization} bytes; lower the output limit or excerpt_bytes"
            ),
        ));
    }

    // Inputs always precede their consumers, so one reverse pass finds every
    // node the output depends on.
    let mut used = vec![false; nodes.len()];
    used[output] = true;
    for i in (0..nodes.len()).rev() {
        if used[i] {
            for dep in nodes[i].op.inputs() {
                used[dep] = true;
            }
        }
    }
    if let Some(i) = used.iter().position(|u| !u) {
        return Err(invalid(
            &format!("nodes[{i}]"),
            format!("node \"{}\" does not feed the output", nodes[i].id),
        ));
    }

    // Split max_edges_returned across neighbors nodes without an explicit max_edges.
    let budget = limits.max_edges_returned as u64;
    let (mut explicit, mut defaults) = (0u64, 0u64);
    for n in &nodes {
        if let Op::Neighbors { max_edges, .. } = n.op {
            if max_edges == 0 {
                defaults += 1;
            } else {
                explicit += max_edges as u64;
            }
        }
    }
    let share = if defaults == 0 {
        0
    } else {
        budget.saturating_sub(explicit) / defaults
    };
    if explicit > budget || (defaults > 0 && share == 0) {
        return Err(limit_exceeded(
            "max_edges_returned",
            budget,
            "neighbors max_edges exceed max_edges_returned for the plan",
        ));
    }
    for n in &mut nodes {
        if let Op::Neighbors { max_edges, .. } = &mut n.op {
            if *max_edges == 0 {
                *max_edges = share as u32;
            }
        }
    }
    Ok(Plan {
        nodes,
        output,
        excerpt_bytes,
        max_response_bytes,
    })
}

// ---------------------------------------------------------------------------
// Compilation

#[derive(Default)]
struct Binder {
    params: Vec<Param>,
}

impl Binder {
    fn bind(&mut self, p: Param) -> String {
        self.params.push(p);
        format!("${}", self.params.len())
    }
}

pub(crate) struct Tables {
    pub spans: String,
    pub versions: String,
    pub assets: String,
    pub publications: String,
    pub tombstones: String,
    pub relations: String,
    pub tags: String,
    pub embeddings: String,
}

impl Tables {
    pub fn new(corpus: &str) -> Tables {
        let c = quote_ident(corpus);
        let t = |name: &str| format!("{c}.\"{name}\"");
        Tables {
            spans: t("spans"),
            versions: t("versions"),
            assets: t("assets"),
            publications: t("publications"),
            tombstones: t("tombstones"),
            relations: t("relations"),
            tags: t("tags"),
            embeddings: t("embeddings"),
        }
    }

    /// Joins version, asset, publication and tombstone rows for span alias `s`.
    pub fn status_joins(&self, s: &str, v: &str, a: &str, p: &str, t: &str) -> String {
        format!(
            "JOIN {versions} {v} ON {v}.version_id {EQ} {s}.version_id \
             LEFT JOIN {assets} {a} ON {a}.asset_id {EQ} {v}.asset_id \
             LEFT JOIN {publications} {p} ON {p}.version_id {EQ} {v}.version_id \
             LEFT JOIN {tombstones} {t} ON {t}.version_id {EQ} {v}.version_id",
            versions = self.versions,
            assets = self.assets,
            publications = self.publications,
            tombstones = self.tombstones,
        )
    }
}

/// Endpoint status from docs/api.md; purge takes precedence.
pub(crate) fn status_case(v: &str, a: &str, p: &str, t: &str) -> String {
    format!(
        "CASE WHEN {t}.version_id IS NOT NULL THEN 'purged' \
         WHEN {a}.current_version_id {EQ} {v}.version_id THEN 'current' \
         WHEN {p}.version_id IS NULL THEN 'staged' \
         WHEN {a}.current_version_id IS NULL AND {a}.retired_at IS NOT NULL THEN 'retired' \
         ELSE 'historical' END"
    )
}

fn filter_sql(f: &Filter, t: &Tables, b: &mut Binder) -> String {
    let mut sql = String::new();
    if let Some(prefix) = &f.path_prefix {
        sql += &format!(
            " AND pg_catalog.starts_with(v.path, {})",
            b.bind(Param::Text(prefix.clone()))
        );
    }
    if !f.asset_ids.is_empty() {
        sql += &format!(
            " AND v.asset_id {EQ} ANY ({}::pg_catalog.uuid[])",
            b.bind(Param::UuidArray(f.asset_ids.clone()))
        );
    }
    if !f.tags_all.is_empty() {
        sql += &format!(
            " AND NOT EXISTS (SELECT 1 FROM pg_catalog.unnest({}::pg_catalog.text[]) AS w(tag) \
             WHERE NOT EXISTS (SELECT 1 FROM {tags} g WHERE g.asset_id {EQ} v.asset_id AND g.tag {EQ} w.tag))",
            b.bind(Param::TextArray(f.tags_all.clone())),
            tags = t.tags
        );
    }
    if !f.tags_any.is_empty() {
        sql += &format!(
            " AND EXISTS (SELECT 1 FROM {tags} g WHERE g.asset_id {EQ} v.asset_id \
             AND g.tag {EQ} ANY ({}::pg_catalog.text[]))",
            b.bind(Param::TextArray(f.tags_any.clone())),
            tags = t.tags
        );
    }
    sql
}

fn search_cte(i: usize, node: &Node, ctx: &QueryContext, t: &Tables, b: &mut Binder) -> String {
    let current = format!(
        "JOIN {assets} a ON a.current_version_id {EQ} s.version_id \
         JOIN {versions} v ON v.version_id {EQ} s.version_id",
        assets = t.assets,
        versions = t.versions
    );
    let by_position = (
        "v.path, s.start_byte, s.evidence_id",
        "q.path, q.start_byte, q.evidence_id",
    );
    let (from, score, predicate, filter, (inner, outer)): (
        String,
        String,
        String,
        &Filter,
        (String, String),
    );
    match &node.op {
        Op::Literal { text, filter: f } => {
            from = format!("{} s {current}", t.spans);
            score = "NULL::pg_catalog.float8".into();
            predicate = format!(
                "pg_catalog.strpos(s.text, {}) {GT} 0",
                b.bind(Param::Text(text.clone()))
            );
            filter = f;
            (inner, outer) = (by_position.0.into(), by_position.1.into());
        }
        Op::Regex {
            pattern,
            case_insensitive,
            filter: f,
        } => {
            from = format!("{} s {current}", t.spans);
            score = "NULL::pg_catalog.float8".into();
            let op = if *case_insensitive { "~*" } else { "~" };
            predicate = format!(
                "s.text OPERATOR(pg_catalog.{op}) {}",
                b.bind(Param::Text(pattern.clone()))
            );
            filter = f;
            (inner, outer) = (by_position.0.into(), by_position.1.into());
        }
        Op::Lexical {
            query,
            syntax,
            filter: f,
        } => {
            let func = match syntax {
                Syntax::Websearch => "websearch_to_tsquery",
                Syntax::Plain => "plainto_tsquery",
                Syntax::Phrase => "phraseto_tsquery",
            };
            from = format!(
                "{} s {current} CROSS JOIN (SELECT pg_catalog.{func}({}::pg_catalog.regconfig, {}) AS query) tq",
                t.spans,
                quote_literal(ctx.text_search_config),
                b.bind(Param::Text(query.clone()))
            );
            score = "pg_catalog.ts_rank_cd(s.tsv, tq.query)::pg_catalog.float8".into();
            predicate = "s.tsv OPERATOR(pg_catalog.@@) tq.query".into();
            filter = f;
            (inner, outer) = (
                "score DESC, s.evidence_id".into(),
                "q.score DESC, q.evidence_id".into(),
            );
        }
        Op::Semantic { vector, filter: f } => {
            // parse_plan guarantees both are present for a semantic node.
            let vs = quote_ident(ctx.vector_schema.unwrap_or_default());
            let dims = ctx.embedding.map(|e| e.dimensions).unwrap_or_default();
            let q = b.bind(Param::Float4Array(vector.clone()));
            let distance = format!(
                "(e.embedding OPERATOR({vs}.<=>) {q}::pg_catalog.float4[]::{vs}.vector({dims}))"
            );
            from = format!(
                "{} e JOIN {} s ON s.evidence_id {EQ} e.evidence_id {current}",
                t.embeddings, t.spans
            );
            score = distance.clone();
            predicate = "true".into();
            filter = f;
            (inner, outer) = (distance, "q.score, q.evidence_id".into());
        }
        Op::Neighbors { .. } | Op::Union { .. } => unreachable!("not a search node"),
    }
    let filter = filter_sql(filter, t, b);
    format!(
        "n{i} AS MATERIALIZED (SELECT q.evidence_id, q.score, '{mode}'::pg_catalog.text AS mode, \
         pg_catalog.row_number() OVER (ORDER BY {outer}) AS rn FROM (\
         SELECT s.evidence_id, {score} AS score, v.path, s.start_byte FROM {from} \
         WHERE {predicate}{filter} ORDER BY {inner} LIMIT {limit}) q)",
        mode = node.op.name(),
        limit = node.limit as u64 + 1
    )
}

fn neighbor_ctes(i: usize, node: &Node, plan: &Plan, t: &Tables, b: &mut Binder) -> [String; 2] {
    let Op::Neighbors {
        from,
        direction,
        kinds,
        statuses,
        max_edges,
    } = &node.op
    else {
        unreachable!("not a neighbors node")
    };
    let from_limit = plan.nodes[*from].limit;
    let out = format!(
        "SELECT f.rn AS from_rn, r.kind, r.target_evidence_id AS to_id, 0 AS dir, \
         r.source_evidence_id AS source_id, r.target_evidence_id AS target_id \
         FROM n{from} f JOIN {rel} r ON r.source_evidence_id {EQ} f.evidence_id WHERE f.rn {LE} {from_limit}",
        rel = t.relations
    );
    let inbound = format!(
        "SELECT f.rn AS from_rn, r.kind, r.source_evidence_id AS to_id, 1 AS dir, \
         r.source_evidence_id AS source_id, r.target_evidence_id AS target_id \
         FROM n{from} f JOIN {rel} r ON r.target_evidence_id {EQ} f.evidence_id WHERE f.rn {LE} {from_limit}",
        rel = t.relations
    );
    let edges = match direction {
        Direction::Out => out,
        Direction::In => inbound,
        Direction::Both => format!("{out} UNION ALL {inbound}"),
    };
    let mut conditions = vec!["true".to_string()];
    if !kinds.is_empty() {
        conditions.push(format!(
            "x.kind {EQ} ANY ({}::pg_catalog.text[])",
            b.bind(Param::TextArray(kinds.clone()))
        ));
    }
    if !statuses.is_empty() {
        conditions.push(format!(
            "(SELECT {case} FROM {spans} s2 {joins} WHERE s2.evidence_id {EQ} x.to_id) {EQ} ANY ({}::pg_catalog.text[])",
            b.bind(Param::TextArray(statuses.clone())),
            case = status_case("v2", "a2", "p2", "t2"),
            spans = t.spans,
            joins = t.status_joins("s2", "v2", "a2", "p2", "t2"),
        ));
    }
    let order = "from_rn, kind, to_id, dir";
    [
        format!(
            "e{i} AS MATERIALIZED (SELECT z.*, pg_catalog.row_number() OVER (ORDER BY {zorder}) AS edge_rn FROM (\
             SELECT x.* FROM ({edges}) x WHERE {cond} ORDER BY {xorder} LIMIT {limit}) z)",
            zorder = order.split(", ").map(|c| format!("z.{c}")).collect::<Vec<_>>().join(", "),
            xorder = order.split(", ").map(|c| format!("x.{c}")).collect::<Vec<_>>().join(", "),
            cond = conditions.join(" AND "),
            limit = *max_edges as u64 + 1
        ),
        format!(
            "n{i} AS MATERIALIZED (SELECT q.evidence_id, NULL::pg_catalog.float8 AS score, \
             'neighbors'::pg_catalog.text AS mode, pg_catalog.row_number() OVER (ORDER BY q.first_edge) AS rn FROM (\
             SELECT e.to_id AS evidence_id, pg_catalog.min(e.edge_rn) AS first_edge FROM e{i} e \
             WHERE e.edge_rn {LE} {max_edges} GROUP BY e.to_id ORDER BY first_edge LIMIT {limit}) q)",
            limit = node.limit as u64 + 1
        ),
    ]
}

fn union_cte(i: usize, node: &Node, plan: &Plan) -> String {
    let Op::Union { inputs } = &node.op else {
        unreachable!("not a union node")
    };
    let parts: Vec<String> = inputs
        .iter()
        .enumerate()
        .map(|(k, n)| {
            format!(
                "SELECT evidence_id, score, mode, {k} AS inp, rn AS irn FROM n{n} WHERE rn {LE} {}",
                plan.nodes[*n].limit
            )
        })
        .collect();
    format!(
        "n{i} AS MATERIALIZED (SELECT q.evidence_id, q.score, q.mode, \
         pg_catalog.row_number() OVER (ORDER BY q.inp, q.irn) AS rn FROM (\
         SELECT DISTINCT ON (x.evidence_id) x.evidence_id, x.score, x.mode, x.inp, x.irn FROM ({}) x \
         ORDER BY x.evidence_id, x.inp, x.irn) q ORDER BY q.inp, q.irn LIMIT {})",
        parts.join(" UNION ALL "),
        node.limit as u64 + 1
    )
}

/// One statement returning one row: `results`, `edges` and `stats` as json
/// arrays (see render.rs for their element layout).
pub fn compile(plan: &Plan, ctx: &QueryContext) -> Compiled {
    let t = Tables::new(ctx.corpus);
    let mut b = Binder::default();
    let mut ctes = Vec::with_capacity(plan.nodes.len() + 4);
    for (i, node) in plan.nodes.iter().enumerate() {
        match node.op {
            Op::Neighbors { .. } => ctes.extend(neighbor_ctes(i, node, plan, &t, &mut b)),
            Op::Union { .. } => ctes.push(union_cte(i, node, plan)),
            _ => ctes.push(search_cte(i, node, ctx, &t, &mut b)),
        }
    }

    let out = plan.output;
    let results = format!(
        "(SELECT pg_catalog.json_agg(pg_catalog.json_build_array(o.evidence_id, s.version_id, v.asset_id, \
         v.path, s.start_byte, s.end_byte, pg_catalog.left(s.text, {excerpt}), o.mode, {status}, o.score) \
         ORDER BY o.rn) FROM n{out} o JOIN {spans} s ON s.evidence_id {EQ} o.evidence_id {joins} \
         WHERE o.rn {LE} {limit})",
        excerpt = plan.excerpt_bytes,
        status = status_case("v", "a", "p", "t"),
        spans = t.spans,
        joins = t.status_joins("s", "v", "a", "p", "t"),
        limit = plan.nodes[out].limit
    );

    let mut edge_parts = Vec::new();
    let mut stat_parts = Vec::with_capacity(plan.nodes.len());
    for (i, node) in plan.nodes.iter().enumerate() {
        let edge_count = if let Op::Neighbors { max_edges, .. } = node.op {
            edge_parts.push(format!(
                "SELECT {i} AS node, e.edge_rn, e.source_id, e.kind, e.target_id, e.to_id FROM e{i} e \
                 WHERE e.edge_rn {LE} {max_edges}"
            ));
            format!("(SELECT pg_catalog.count(*) FROM e{i})")
        } else {
            "0::pg_catalog.int8".to_string()
        };
        stat_parts.push(format!(
            "SELECT {i} AS i, (SELECT pg_catalog.count(*) FROM n{i}) AS n, {edge_count} AS e"
        ));
    }
    let edges = if edge_parts.is_empty() {
        "NULL::pg_catalog.json".to_string()
    } else {
        format!(
            "(SELECT pg_catalog.json_agg(pg_catalog.json_build_array(x.node, x.source_id, x.kind, x.target_id, x.to_id) \
             ORDER BY x.node, x.edge_rn) FROM ({}) x)",
            edge_parts.join(" UNION ALL ")
        )
    };
    let stats = format!(
        "(SELECT pg_catalog.json_agg(pg_catalog.json_build_array(z.i, z.n, z.e) ORDER BY z.i) FROM ({}) z)",
        stat_parts.join(" UNION ALL ")
    );
    Compiled {
        sql: format!(
            "WITH {} SELECT {results} AS results, {edges} AS edges, {stats} AS stats",
            ctes.join(", ")
        ),
        params: b.params,
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::error::SqlState;
    use crate::model::{Embedding, Limits};
    use serde_json::json;

    fn ctx_with<'a>(
        emb: Option<&'a Embedding>,
        vs: Option<&'a str>,
        limits: &'a Limits,
    ) -> QueryContext<'a> {
        QueryContext {
            corpus: "docs",
            text_search_config: "pg_catalog.english",
            vector_schema: vs,
            embedding: emb,
            limits,
        }
    }

    fn compile_json(v: Value) -> ApiResult<Compiled> {
        let limits = Limits::DEFAULT;
        let ctx = ctx_with(None, None, &limits);
        parse_plan(&v, &ctx).map(|p| compile(&p, &ctx))
    }

    fn err(v: Value) -> (SqlState, String) {
        let e = compile_json(v).expect_err("expected error");
        (e.state, e.reason().to_string())
    }

    /// Removes qualified operators; whatever operator characters remain are unqualified.
    fn bare_operators(sql: &str, vs: Option<&str>) -> Vec<char> {
        let mut s = sql.to_string();
        for op in ["=", "<=", "<", ">", "~*", "~", "@@"] {
            s = s.replace(&format!("OPERATOR(pg_catalog.{op})"), "");
        }
        if let Some(vs) = vs {
            s = s.replace(&format!("OPERATOR({}.<=>)", quote_ident(vs)), "");
        }
        s.chars().filter(|c| "=<>~@!".contains(*c)).collect()
    }

    #[test]
    fn single_literal_node_compiles_with_parameters_only() {
        let hostile = "x'; DROP TABLE docs.versions; --";
        let c = compile_json(
            json!({"nodes": [{"id": "a", "op": "literal", "text": hostile}], "output": "a"}),
        )
        .unwrap();
        assert!(!c.sql.contains("DROP"), "{}", c.sql);
        assert!(c.params.contains(&Param::Text(hostile.into())));
        assert!(c.sql.contains("\"docs\".\"spans\""));
        assert!(c.sql.contains("AS MATERIALIZED"));
        assert!(bare_operators(&c.sql, None).is_empty(), "{}", c.sql);
        assert_eq!(c.sql.matches("SELECT").count() >= 1, true);
        assert!(!c.sql.contains(';'));
    }

    #[test]
    fn composed_plan_compiles_into_one_statement() {
        let plan = json!({
            "nodes": [
                {"id": "hits", "op": "lexical", "query": "retry semantics", "limit": 5,
                 "filter": {"path_prefix": "docs/", "tags_all": ["v1"], "tags_any": ["a", "b"],
                            "asset_ids": ["0b5f3c1e-8d2a-4f6b-9c3d-2e1f0a9b8c7d"]}},
                {"id": "rx", "op": "regex", "pattern": "^Retry", "case_insensitive": true},
                {"id": "near", "op": "neighbors", "from": "hits", "direction": "both",
                 "kinds": ["cites"], "status": ["current", "historical"], "limit": 20},
                {"id": "all", "op": "union", "inputs": ["hits", "rx", "near"], "limit": 30}
            ],
            "output": "all",
            "excerpt_bytes": 100
        });
        let c = compile_json(plan).unwrap();
        for needle in [
            "n0 AS MATERIALIZED",
            "n1 AS MATERIALIZED",
            "e2 AS MATERIALIZED",
            "n2 AS MATERIALIZED",
            "n3 AS MATERIALIZED",
            "OPERATOR(pg_catalog.@@)",
            "OPERATOR(pg_catalog.~*)",
            "'pg_catalog.english'::pg_catalog.regconfig",
            "pg_catalog.websearch_to_tsquery",
            "\"docs\".\"relations\"",
            "\"docs\".\"tags\"",
        ] {
            assert!(c.sql.contains(needle), "missing {needle}: {}", c.sql);
        }
        assert!(
            bare_operators(&c.sql, None).is_empty(),
            "{:?}\n{}",
            bare_operators(&c.sql, None),
            c.sql
        );
        assert!(!c.sql.contains("retry semantics") && !c.sql.contains("^Retry"));
        assert!(!c.sql.contains(';'));
    }

    #[test]
    fn semantic_uses_qualified_pgvector() {
        let emb = Embedding {
            model: "m1".into(),
            dimensions: 3,
        };
        let limits = Limits::DEFAULT;
        let vs = "ext \"vec\"";
        let ctx = ctx_with(Some(&emb), Some(vs), &limits);
        let p = parse_plan(&json!({"nodes": [{"id": "s", "op": "semantic", "model": "m1", "vector": [1, 0, 0]}], "output": "s"}), &ctx).unwrap();
        let c = compile(&p, &ctx);
        assert!(
            c.sql.contains("OPERATOR(\"ext \"\"vec\"\"\".<=>)"),
            "{}",
            c.sql
        );
        assert!(
            c.sql.contains("::\"ext \"\"vec\"\"\".vector(3)"),
            "{}",
            c.sql
        );
        assert!(c.params.contains(&Param::Float4Array(vec![1.0, 0.0, 0.0])));
        assert!(bare_operators(&c.sql, Some(vs)).is_empty(), "{}", c.sql);
        assert!(p.approximate());

        for (bad, state) in [
            (
                json!({"nodes": [{"id": "s", "op": "semantic", "model": "m1", "vector": [1, 0]}], "output": "s"}),
                SqlState::InvalidParameter,
            ),
            (
                json!({"nodes": [{"id": "s", "op": "semantic", "model": "m2", "vector": [1, 0, 0]}], "output": "s"}),
                SqlState::InvalidParameter,
            ),
            (
                json!({"nodes": [{"id": "s", "op": "semantic", "model": "m1", "vector": [0, 0, 0]}], "output": "s"}),
                SqlState::InvalidParameter,
            ),
        ] {
            assert_eq!(parse_plan(&bad, &ctx).unwrap_err().state, state);
        }
        // Configured embeddings but pgvector missing at call time.
        let ctx = ctx_with(Some(&emb), None, &limits);
        let e = parse_plan(&json!({"nodes": [{"id": "s", "op": "semantic", "model": "m1", "vector": [1, 0, 0]}], "output": "s"}), &ctx).unwrap_err();
        assert_eq!(
            (e.state, e.reason()),
            (SqlState::Prerequisite, "pgvector_missing")
        );
    }

    #[test]
    fn semantic_without_embeddings_is_rejected() {
        assert_eq!(
            err(
                json!({"nodes": [{"id": "s", "op": "semantic", "model": "m", "vector": [1]}], "output": "s"})
            ),
            (SqlState::InvalidParameter, "semantic_not_configured".into())
        );
    }

    #[test]
    fn plan_structure_is_validated() {
        let lit = |id: &str| json!({"id": id, "op": "literal", "text": "x"});
        let cases = vec![
            json!([]),
            json!({"output": "a"}),
            json!({"nodes": [], "output": "a"}),
            json!({"nodes": [lit("a")]}),
            json!({"nodes": [lit("a")], "output": "b"}),
            json!({"nodes": [lit("a"), lit("a")], "output": "a"}),
            json!({"nodes": [lit("A")], "output": "A"}),
            json!({"nodes": [lit("a\"b")], "output": "a\"b"}),
            json!({"nodes": [lit("a"), lit("b")], "output": "a"}), // unused node
            json!({"nodes": [{"id": "n", "op": "neighbors", "from": "a"}, lit("a")], "output": "n"}), // forward ref
            json!({"nodes": [{"id": "n", "op": "neighbors", "from": "n"}], "output": "n"}), // self ref
            json!({"nodes": [lit("a"), {"id": "u", "op": "union", "inputs": ["a"]}], "output": "u"}),
            json!({"nodes": [lit("a"), {"id": "u", "op": "union", "inputs": ["a", "a"]}], "output": "u"}),
            json!({"nodes": [{"id": "a", "op": "sql", "text": "x"}], "output": "a"}),
            json!({"nodes": [{"id": "a", "op": "literal", "text": "x", "sql": "1"}], "output": "a"}),
            json!({"nodes": [{"id": "a", "op": "literal", "text": ""}], "output": "a"}),
            json!({"nodes": [{"id": "a", "op": "regex", "pattern": "x".repeat(1025)}], "output": "a"}),
            json!({"nodes": [{"id": "a", "op": "lexical", "query": "x", "syntax": "raw"}], "output": "a"}),
            json!({"nodes": [{"id": "a", "op": "literal", "text": "x", "limit": 0}], "output": "a"}),
            json!({"nodes": [{"id": "a", "op": "literal", "text": "x", "filter": {"sql": "1"}}], "output": "a"}),
            json!({"nodes": [lit("a"), {"id": "n", "op": "neighbors", "from": "a", "status": ["deleted"]}], "output": "n"}),
            json!({"nodes": [lit("a"), {"id": "n", "op": "neighbors", "from": "a", "direction": "up"}], "output": "n"}),
            json!({"nodes": [lit("a"), {"id": "n", "op": "neighbors", "from": "a", "kinds": ["Bad"]}], "output": "n"}),
            json!({"nodes": [lit("a")], "output": "a", "extra": 1}),
        ];
        for c in cases {
            assert_eq!(err(c.clone()).0, SqlState::InvalidParameter, "{c}");
        }
    }

    #[test]
    fn budgets_are_enforced_as_limits() {
        let lit = |id: String| json!({"id": id, "op": "literal", "text": "x"});
        // 33 nodes > max_plan_nodes 32.
        let mut nodes: Vec<Value> = (0..32).map(|i| lit(format!("a{i}"))).collect();
        let inputs: Vec<String> = (0..32).map(|i| format!("a{i}")).collect();
        nodes.push(json!({"id": "u", "op": "union", "inputs": inputs}));
        assert_eq!(
            err(json!({"nodes": nodes, "output": "u"})).0,
            SqlState::ProgramLimit
        );

        for c in [
            json!({"nodes": [{"id": "a", "op": "literal", "text": "x", "limit": 257}], "output": "a"}),
            json!({"nodes": [{"id": "a", "op": "literal", "text": "x"}], "output": "a", "max_response_bytes": 65537}),
            json!({"nodes": [{"id": "a", "op": "literal", "text": "x"}], "output": "a", "excerpt_bytes": 65537}),
            json!({"nodes": [{"id": "a", "op": "literal", "text": "x"},
                             {"id": "n", "op": "neighbors", "from": "a", "max_edges": 1025}], "output": "n"}),
            json!({"nodes": [{"id": "a", "op": "literal", "text": "x"},
                             {"id": "n", "op": "neighbors", "from": "a", "max_edges": 600},
                             {"id": "m", "op": "neighbors", "from": "a", "max_edges": 600},
                             {"id": "u", "op": "union", "inputs": ["n", "m"]}], "output": "u"}),
        ] {
            assert_eq!(
                err(c.clone()),
                (SqlState::ProgramLimit, "limit_exceeded".into()),
                "{c}"
            );
        }
    }

    #[test]
    fn defaults_and_edge_split() {
        let limits = Limits::DEFAULT;
        let ctx = ctx_with(None, None, &limits);
        let p = parse_plan(
            &json!({"nodes": [
            {"id": "a", "op": "literal", "text": "x"},
            {"id": "n", "op": "neighbors", "from": "a"},
            {"id": "m", "op": "neighbors", "from": "a", "max_edges": 24},
            {"id": "u", "op": "union", "inputs": ["n", "m"]}
        ], "output": "u"}),
            &ctx,
        )
        .unwrap();
        assert_eq!(p.nodes[0].limit, DEFAULT_LIMIT);
        assert_eq!(p.excerpt_bytes, DEFAULT_EXCERPT_BYTES);
        assert_eq!(p.max_response_bytes, 64 << 10);
        assert_eq!(p.nodes[3].limit, 20); // sum of inputs, capped at max_candidates
        match (&p.nodes[1].op, &p.nodes[2].op) {
            (Op::Neighbors { max_edges: a, .. }, Op::Neighbors { max_edges: b, .. }) => {
                assert_eq!((*a, *b), (1000, 24));
            }
            other => panic!("{other:?}"),
        }
        assert!(!p.approximate());
    }

    #[test]
    fn excerpt_materialization_is_capped() {
        let limits = Limits {
            max_response_bytes: 16 << 20,
            max_candidates_per_operator: 10_000,
            ..Limits::DEFAULT
        };
        let ctx = ctx_with(None, None, &limits);
        let plan = |limit: u64, excerpt: u64| {
            json!({"nodes": [{"id": "a", "op": "literal", "text": "x", "limit": limit}],
                   "output": "a", "excerpt_bytes": excerpt})
        };
        // Exactly 64 MiB: 6 x 4096 x 2731 = 67,117,056 > 67,108,864; 6 x 4096 x 2730 fits.
        parse_plan(&plan(4096, 2730), &ctx).unwrap();
        let e = parse_plan(&plan(4096, 2731), &ctx).unwrap_err();
        assert_eq!(
            (e.state, e.detail["limit"].as_str()),
            (
                SqlState::ProgramLimit,
                Some("max_excerpt_materialization_bytes")
            )
        );
        // 1 x 11,184,810 bytes (= floor(64 MiB / 6)) fits; one more byte does not.
        parse_plan(&plan(1, 11_184_810), &ctx).unwrap();
        assert_eq!(
            parse_plan(&plan(1, 11_184_811), &ctx).unwrap_err().state,
            SqlState::ProgramLimit
        );
        let huge = parse_plan(&plan(10_000, 16 << 20), &ctx).unwrap_err();
        assert_eq!(huge.detail["limit"], "max_excerpt_materialization_bytes");
        // Small queries against a collection with a large response budget are
        // unaffected; the default excerpt stays 512 bytes.
        let p = parse_plan(
            &json!({"nodes": [{"id": "a", "op": "literal", "text": "x"}], "output": "a"}),
            &ctx,
        )
        .unwrap();
        assert_eq!(p.excerpt_bytes, DEFAULT_EXCERPT_BYTES);
        // Only the output node counts: a large intermediate limit with a small output is fine.
        parse_plan(
            &json!({"nodes": [
            {"id": "a", "op": "literal", "text": "x", "limit": 10_000},
            {"id": "b", "op": "literal", "text": "y", "limit": 10},
            {"id": "u", "op": "union", "inputs": ["a", "b"], "limit": 10}
        ], "output": "u", "excerpt_bytes": 1 << 20}),
            &ctx,
        )
        .unwrap();
    }

    #[test]
    fn identifier_and_literal_quoting() {
        assert_eq!(quote_ident("docs"), "\"docs\"");
        assert_eq!(quote_ident("a\"b"), "\"a\"\"b\"");
        assert_eq!(quote_literal("pg_catalog.english"), "'pg_catalog.english'");
        assert_eq!(quote_literal("it's"), "'it''s'");
        assert_eq!(quote_literal("a\\b"), "E'a\\\\b'");
    }
}
