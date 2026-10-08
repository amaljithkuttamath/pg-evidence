//! Validation of JSON requests for the mutating API and collection config
//! (docs/api.md). Pure Rust: no pgrx, so it is unit tested on the host.
//! Every object rejects unknown fields; failures are 22023 unless a size limit
//! is exceeded (54000).

use crate::error::{invalid, limit_exceeded, ApiError, ApiResult, SqlState};
use serde_json::{Map, Value};
use sha2::{Digest, Sha256};
use std::collections::HashSet;
use std::fmt;

pub const SCHEMA_VERSION: i32 = 1;
/// Fixed protective limit on spans per staged version (54000 when exceeded).
pub const MAX_SPANS_PER_VERSION: usize = 65_536;
/// Overlapping spans copy source bytes; their total is capped at this multiple
/// of max_source_bytes (54000, limit `max_span_bytes_total`).
pub const SPAN_BYTES_FACTOR: u64 = 4;
/// Fixed cap on vectors x dimensions per attach_embeddings call (4 MiB of float4).
pub const MAX_EMBEDDING_VALUES_PER_CALL: u64 = 1 << 20;
pub const MAX_PATH_BYTES: usize = 1024;
pub const MAX_REASON_BYTES: usize = 1024;
pub const MAX_KEY_BYTES: usize = 256;
pub const MAX_TAG_BYTES: usize = 128;
pub const MAX_TAGS_PER_CALL: usize = 64;
pub const MAX_MODEL_BYTES: usize = 256;
/// pgvector's HNSW limit for `vector`.
pub const MAX_EMBEDDING_DIMENSIONS: u64 = 2000;

// ---------------------------------------------------------------------------
// JSON field helpers shared with plan.rs

fn join(path: &str, key: &str) -> String {
    if path.is_empty() {
        key.to_string()
    } else {
        format!("{path}.{key}")
    }
}

fn label(path: &str) -> &str {
    if path.is_empty() {
        "request"
    } else {
        path
    }
}

/// The value as an object whose keys are all in `allowed`.
pub(crate) fn object<'a>(
    v: &'a Value,
    path: &str,
    allowed: &[&str],
) -> ApiResult<&'a Map<String, Value>> {
    let map = v.as_object().ok_or_else(|| {
        invalid(
            label(path),
            format!("{} must be a JSON object", label(path)),
        )
    })?;
    for key in map.keys() {
        if !allowed.contains(&key.as_str()) {
            return Err(invalid(
                &join(path, key),
                format!("unknown field \"{key}\" in {}", label(path)),
            ));
        }
    }
    Ok(map)
}

/// A present, non-null field.
pub(crate) fn required<'a>(
    m: &'a Map<String, Value>,
    path: &str,
    key: &str,
) -> ApiResult<&'a Value> {
    match m.get(key) {
        Some(Value::Null) | None => Err(invalid(
            &join(path, key),
            format!("{} is required", join(path, key)),
        )),
        Some(v) => Ok(v),
    }
}

/// An absent or null field is `None`.
pub(crate) fn optional<'a>(m: &'a Map<String, Value>, key: &str) -> Option<&'a Value> {
    m.get(key).filter(|v| !v.is_null())
}

pub(crate) fn string<'a>(v: &'a Value, path: &str) -> ApiResult<&'a str> {
    v.as_str()
        .ok_or_else(|| invalid(path, format!("{path} must be a string")))
}

pub(crate) fn boolean(v: &Value, path: &str) -> ApiResult<bool> {
    v.as_bool()
        .ok_or_else(|| invalid(path, format!("{path} must be a boolean")))
}

pub(crate) fn array<'a>(v: &'a Value, path: &str) -> ApiResult<&'a Vec<Value>> {
    v.as_array()
        .ok_or_else(|| invalid(path, format!("{path} must be an array")))
}

/// A non-negative JSON integer no larger than `max`.
pub(crate) fn uint(v: &Value, path: &str, max: u64) -> ApiResult<u64> {
    match v.as_u64() {
        Some(n) if n <= max => Ok(n),
        _ => Err(invalid(
            path,
            format!("{path} must be an integer from 0 to {max}"),
        )),
    }
}

/// A non-empty string of at most `max_bytes` bytes without control characters.
pub(crate) fn text(v: &Value, path: &str, max_bytes: usize) -> ApiResult<String> {
    let s = string(v, path)?;
    if s.is_empty() || s.len() > max_bytes || s.chars().any(char::is_control) {
        return Err(invalid(
            path,
            format!("{path} must be 1 to {max_bytes} bytes without control characters"),
        ));
    }
    Ok(s.to_string())
}

/// `^[a-z][a-z0-9_]{0,max-1}$`
pub(crate) fn is_simple_name(s: &str, max: usize) -> bool {
    let b = s.as_bytes();
    !b.is_empty()
        && b.len() <= max
        && b[0].is_ascii_lowercase()
        && b.iter()
            .all(|c| c.is_ascii_lowercase() || c.is_ascii_digit() || *c == b'_')
}

pub fn hex(bytes: &[u8]) -> String {
    const DIGITS: &[u8; 16] = b"0123456789abcdef";
    let mut s = String::with_capacity(bytes.len() * 2);
    for b in bytes {
        s.push(DIGITS[(b >> 4) as usize] as char);
        s.push(DIGITS[(b & 15) as usize] as char);
    }
    s
}

pub fn sha256(bytes: &[u8]) -> [u8; 32] {
    let digest = Sha256::digest(bytes);
    let mut out = [0u8; 32];
    out.copy_from_slice(&digest);
    out
}

fn lower_hex_digest(v: &Value, path: &str) -> ApiResult<[u8; 32]> {
    let s = string(v, path)?;
    let b = s.as_bytes();
    if b.len() != 64
        || !b
            .iter()
            .all(|c| c.is_ascii_digit() || (b'a'..=b'f').contains(c))
    {
        return Err(invalid(
            path,
            format!("{path} must be 64 lowercase hexadecimal digits"),
        ));
    }
    let mut out = [0u8; 32];
    for (i, pair) in b.chunks(2).enumerate() {
        let nib = |c: u8| {
            if c.is_ascii_digit() {
                c - b'0'
            } else {
                c - b'a' + 10
            }
        };
        out[i] = (nib(pair[0]) << 4) | nib(pair[1]);
    }
    Ok(out)
}

#[derive(Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash, Debug)]
pub struct Uuid16(pub [u8; 16]);

impl Uuid16 {
    /// Hyphenated form, either case.
    pub fn parse(path: &str, s: &str) -> ApiResult<Self> {
        let b = s.as_bytes();
        let bad = || invalid(path, format!("{path} must be a hyphenated UUID"));
        if b.len() != 36 {
            return Err(bad());
        }
        let mut out = [0u8; 16];
        let mut nibbles = 0usize;
        for (i, &c) in b.iter().enumerate() {
            if matches!(i, 8 | 13 | 18 | 23) {
                if c != b'-' {
                    return Err(bad());
                }
                continue;
            }
            let n = match c {
                b'0'..=b'9' => c - b'0',
                b'a'..=b'f' => c - b'a' + 10,
                b'A'..=b'F' => c - b'A' + 10,
                _ => return Err(bad()),
            };
            out[nibbles / 2] |= if nibbles % 2 == 0 { n << 4 } else { n };
            nibbles += 1;
        }
        Ok(Uuid16(out))
    }

    pub(crate) fn from_value(v: &Value, path: &str) -> ApiResult<Self> {
        Uuid16::parse(path, string(v, path)?)
    }
}

impl fmt::Display for Uuid16 {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        let h = hex(&self.0);
        write!(
            f,
            "{}-{}-{}-{}-{}",
            &h[0..8],
            &h[8..12],
            &h[12..16],
            &h[16..20],
            &h[20..32]
        )
    }
}

// ---------------------------------------------------------------------------
// Collection configuration

pub fn validate_corpus_name(name: &str) -> ApiResult<()> {
    if !is_simple_name(name, 48) {
        return Err(invalid(
            "corpus",
            "corpus name must match ^[a-z][a-z0-9_]{0,47}$",
        ));
    }
    if name.starts_with("pg_") || matches!(name, "information_schema" | "public" | "evidence") {
        return Err(invalid(
            "corpus",
            format!("corpus name \"{name}\" is reserved"),
        ));
    }
    Ok(())
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Limits {
    pub max_source_bytes: u32,
    pub max_response_bytes: u32,
    pub max_candidates_per_operator: u32,
    pub max_plan_nodes: u32,
    pub max_edges_returned: u32,
}

impl Limits {
    /// Protective defaults from docs/design.md, not measured capacities.
    pub const DEFAULT: Limits = Limits {
        max_source_bytes: 1 << 20,
        max_response_bytes: 64 << 10,
        max_candidates_per_operator: 256,
        max_plan_nodes: 32,
        max_edges_returned: 1024,
    };
    /// Upper bounds a collection may configure.
    pub const CEILING: Limits = Limits {
        max_source_bytes: 64 << 20,
        max_response_bytes: 16 << 20,
        max_candidates_per_operator: 10_000,
        max_plan_nodes: 256,
        max_edges_returned: 100_000,
    };

    pub const NAMES: [&'static str; 5] = [
        "max_source_bytes",
        "max_response_bytes",
        "max_candidates_per_operator",
        "max_plan_nodes",
        "max_edges_returned",
    ];

    fn slots(&mut self) -> [&mut u32; 5] {
        [
            &mut self.max_source_bytes,
            &mut self.max_response_bytes,
            &mut self.max_candidates_per_operator,
            &mut self.max_plan_nodes,
            &mut self.max_edges_returned,
        ]
    }
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Embedding {
    pub model: String,
    pub dimensions: u32,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct CollectionConfig {
    pub embedding: Option<Embedding>,
    pub text_search_config: String,
    pub limits: Limits,
}

pub fn parse_config(v: &Value) -> ApiResult<CollectionConfig> {
    let m = object(
        v,
        "config",
        &[
            "embedding_model",
            "embedding_dimensions",
            "text_search_config",
            "limits",
        ],
    )?;
    let model = optional(m, "embedding_model");
    let dims = optional(m, "embedding_dimensions");
    let embedding = match (model, dims) {
        (None, None) => None,
        (Some(model), Some(dims)) => Some(Embedding {
            model: text(model, "config.embedding_model", MAX_MODEL_BYTES)?,
            dimensions: match uint(
                dims,
                "config.embedding_dimensions",
                MAX_EMBEDDING_DIMENSIONS,
            )? {
                0 => {
                    return Err(invalid(
                        "config.embedding_dimensions",
                        "config.embedding_dimensions must be from 1 to 2000",
                    ))
                }
                n => n as u32,
            },
        }),
        _ => {
            return Err(invalid(
                "config.embedding_model",
                "embedding_model and embedding_dimensions must both be set or both be null",
            ))
        }
    };
    let text_search_config = match optional(m, "text_search_config") {
        None => "simple".to_string(),
        Some(v) => text(v, "config.text_search_config", 128)?,
    };
    let mut limits = Limits::DEFAULT;
    if let Some(lv) = optional(m, "limits") {
        let lm = object(lv, "config.limits", &Limits::NAMES)?;
        let ceiling = Limits::CEILING.clone().slots().map(|c| *c);
        for (i, slot) in limits.slots().into_iter().enumerate() {
            let name = Limits::NAMES[i];
            if let Some(x) = optional(lm, name) {
                let path = format!("config.limits.{name}");
                let n = uint(x, &path, ceiling[i] as u64)?;
                if n == 0 {
                    return Err(invalid(&path, format!("{path} must be at least 1")));
                }
                *slot = n as u32;
            }
        }
    }
    Ok(CollectionConfig {
        embedding,
        text_search_config,
        limits,
    })
}

// ---------------------------------------------------------------------------
// stage_version

#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub struct Span {
    pub start: i32,
    pub end: i32,
}

#[derive(Clone, Debug)]
pub struct StageRequest {
    pub asset_id: Uuid16,
    pub path: String,
    pub source: String,
    pub source_sha256: [u8; 32],
    pub spans: Vec<Span>,
    pub ingestion_key: String,
    pub expected_revision: i64,
}

pub fn parse_stage(v: &Value, max_source_bytes: u32) -> ApiResult<StageRequest> {
    let m = object(
        v,
        "",
        &[
            "asset_id",
            "path",
            "source",
            "source_sha256",
            "spans",
            "ingestion_key",
            "expected_revision",
        ],
    )?;
    let asset_id = Uuid16::from_value(required(m, "", "asset_id")?, "asset_id")?;
    let path = text(required(m, "", "path")?, "path", MAX_PATH_BYTES)?;
    let ingestion_key = text(
        required(m, "", "ingestion_key")?,
        "ingestion_key",
        MAX_KEY_BYTES,
    )?;
    let expected_revision = uint(
        required(m, "", "expected_revision")?,
        "expected_revision",
        i64::MAX as u64,
    )? as i64;
    let source = string(required(m, "", "source")?, "source")?;
    if source.len() > max_source_bytes as usize {
        return Err(limit_exceeded(
            "max_source_bytes",
            max_source_bytes as u64,
            format!(
                "source is {} bytes; the collection allows {max_source_bytes}",
                source.len()
            ),
        ));
    }
    let source_sha256 = lower_hex_digest(required(m, "", "source_sha256")?, "source_sha256")?;
    let computed = sha256(source.as_bytes());
    if computed != source_sha256 {
        return Err(ApiError::new(
            SqlState::InvalidParameter,
            "source_sha256_mismatch",
            "source_sha256 does not match the SHA-256 of the received source bytes",
        )
        .with("field", "source_sha256")
        .with("computed", hex(&computed)));
    }

    let span_values = array(required(m, "", "spans")?, "spans")?;
    if span_values.len() > MAX_SPANS_PER_VERSION {
        return Err(limit_exceeded(
            "max_spans_per_version",
            MAX_SPANS_PER_VERSION as u64,
            format!("at most {MAX_SPANS_PER_VERSION} spans per version"),
        ));
    }
    let mut spans = Vec::with_capacity(span_values.len());
    let mut seen = HashSet::with_capacity(span_values.len());
    let max_span_bytes = SPAN_BYTES_FACTOR * max_source_bytes as u64;
    let mut span_bytes = 0u64;
    for (i, sv) in span_values.iter().enumerate() {
        let p = format!("spans[{i}]");
        let sm = object(sv, &p, &["start_byte", "end_byte"])?;
        let start = uint(
            required(sm, &p, "start_byte")?,
            &format!("{p}.start_byte"),
            i32::MAX as u64,
        )? as usize;
        let end = uint(
            required(sm, &p, "end_byte")?,
            &format!("{p}.end_byte"),
            i32::MAX as u64,
        )? as usize;
        if start >= end || end > source.len() {
            return Err(invalid(
                &p,
                format!("{p} must satisfy start_byte < end_byte <= {}", source.len()),
            ));
        }
        if !source.is_char_boundary(start) || !source.is_char_boundary(end) {
            return Err(invalid(
                &p,
                format!("{p} does not fall on UTF-8 character boundaries"),
            ));
        }
        let span = Span {
            start: start as i32,
            end: end as i32,
        };
        if !seen.insert(span) {
            return Err(invalid(&p, format!("{p} duplicates an earlier span")));
        }
        span_bytes += (end - start) as u64;
        if span_bytes > max_span_bytes {
            return Err(limit_exceeded(
                "max_span_bytes_total",
                max_span_bytes,
                format!("spans copy more than {SPAN_BYTES_FACTOR} x max_source_bytes"),
            ));
        }
        spans.push(span);
    }
    Ok(StageRequest {
        asset_id,
        path,
        source: source.to_string(),
        source_sha256,
        spans,
        ingestion_key,
        expected_revision,
    })
}

impl StageRequest {
    pub fn span_text(&self, span: Span) -> &str {
        &self.source[span.start as usize..span.end as usize]
    }

    /// SHA-256 of the canonical request (docs/api.md): the ingestion key is the
    /// lookup key and the source is represented by its verified digest.
    pub fn request_sha256(&self) -> [u8; 32] {
        let mut c = String::with_capacity(160 + self.path.len() + self.spans.len() * 16);
        c.push_str("{\"asset_id\":\"");
        c.push_str(&self.asset_id.to_string());
        c.push_str("\",\"expected_revision\":");
        c.push_str(&self.expected_revision.to_string());
        c.push_str(",\"path\":");
        c.push_str(&Value::String(self.path.clone()).to_string());
        c.push_str(",\"source_sha256\":\"");
        c.push_str(&hex(&self.source_sha256));
        c.push_str("\",\"spans\":[");
        for (i, s) in self.spans.iter().enumerate() {
            if i > 0 {
                c.push(',');
            }
            c.push_str(&format!("[{},{}]", s.start, s.end));
        }
        c.push_str("]}");
        sha256(c.as_bytes())
    }
}

// ---------------------------------------------------------------------------
// attach_embeddings, publish_version, retire, purge

#[derive(Clone, Debug)]
pub struct AttachRequest {
    pub version_id: Uuid16,
    pub vectors: Vec<(Uuid16, Vec<f32>)>,
}

/// Parses a finite, non-zero vector of exactly `dims` values, rounded to float4.
pub(crate) fn vector(v: &Value, path: &str, dims: u32) -> ApiResult<Vec<f32>> {
    let items = array(v, path)?;
    if items.len() != dims as usize {
        return Err(invalid(
            path,
            format!("{path} must have {dims} dimensions, got {}", items.len()),
        ));
    }
    let mut out = Vec::with_capacity(items.len());
    for x in items {
        let f = x
            .as_f64()
            .ok_or_else(|| invalid(path, format!("{path} must contain only numbers")))?
            as f32;
        if !f.is_finite() {
            return Err(invalid(
                path,
                format!("{path} contains a value that is not a finite float4"),
            ));
        }
        out.push(f);
    }
    if out.iter().all(|f| *f == 0.0) {
        return Err(invalid(path, format!("{path} has zero norm")));
    }
    Ok(out)
}

pub fn parse_attach(v: &Value, embedding: &Embedding) -> ApiResult<AttachRequest> {
    let m = object(v, "", &["version_id", "model", "embeddings"])?;
    let version_id = Uuid16::from_value(required(m, "", "version_id")?, "version_id")?;
    let model = string(required(m, "", "model")?, "model")?;
    if model != embedding.model {
        return Err(invalid(
            "model",
            format!(
                "model must be the collection's embedding_model \"{}\"",
                embedding.model
            ),
        ));
    }
    let items = array(required(m, "", "embeddings")?, "embeddings")?;
    if items.is_empty() {
        return Err(invalid("embeddings", "embeddings must not be empty"));
    }
    if items.len() > MAX_SPANS_PER_VERSION {
        return Err(limit_exceeded(
            "max_spans_per_version",
            MAX_SPANS_PER_VERSION as u64,
            "too many embeddings",
        ));
    }
    if items.len() as u64 * embedding.dimensions as u64 > MAX_EMBEDDING_VALUES_PER_CALL {
        return Err(limit_exceeded(
            "max_embedding_values_per_call",
            MAX_EMBEDDING_VALUES_PER_CALL,
            "split the embeddings across several attach_embeddings calls",
        ));
    }
    let mut seen = HashSet::with_capacity(items.len());
    let mut vectors = Vec::with_capacity(items.len());
    for (i, item) in items.iter().enumerate() {
        let p = format!("embeddings[{i}]");
        let im = object(item, &p, &["evidence_id", "vector"])?;
        let id = Uuid16::from_value(
            required(im, &p, "evidence_id")?,
            &format!("{p}.evidence_id"),
        )?;
        if !seen.insert(id) {
            return Err(invalid(&p, format!("{p} repeats evidence_id {id}")));
        }
        vectors.push((
            id,
            vector(
                required(im, &p, "vector")?,
                &format!("{p}.vector"),
                embedding.dimensions,
            )?,
        ));
    }
    Ok(AttachRequest {
        version_id,
        vectors,
    })
}

#[derive(Clone, Debug)]
pub struct PublishRequest {
    pub version_id: Uuid16,
}

pub fn parse_publish(v: &Value) -> ApiResult<PublishRequest> {
    let m = object(v, "", &["version_id"])?;
    Ok(PublishRequest {
        version_id: Uuid16::from_value(required(m, "", "version_id")?, "version_id")?,
    })
}

#[derive(Clone, Debug)]
pub struct RetireRequest {
    pub asset_id: Uuid16,
    pub expected_revision: i64,
}

pub fn parse_retire(v: &Value) -> ApiResult<RetireRequest> {
    let m = object(v, "", &["asset_id", "expected_revision"])?;
    Ok(RetireRequest {
        asset_id: Uuid16::from_value(required(m, "", "asset_id")?, "asset_id")?,
        expected_revision: uint(
            required(m, "", "expected_revision")?,
            "expected_revision",
            i64::MAX as u64,
        )? as i64,
    })
}

#[derive(Clone, Debug)]
pub struct PurgeRequest {
    pub version_id: Uuid16,
    pub reason: String,
}

pub fn parse_purge(v: &Value) -> ApiResult<PurgeRequest> {
    let m = object(v, "", &["version_id", "reason"])?;
    Ok(PurgeRequest {
        version_id: Uuid16::from_value(required(m, "", "version_id")?, "version_id")?,
        reason: text(required(m, "", "reason")?, "reason", MAX_REASON_BYTES)?,
    })
}

// ---------------------------------------------------------------------------
// annotate

#[derive(Clone, Debug)]
pub enum Annotate {
    Tag {
        asset_id: Uuid16,
        tags: Vec<String>,
        expected_annotation_revision: i64,
    },
    Untag {
        asset_id: Uuid16,
        tags: Vec<String>,
        expected_annotation_revision: i64,
    },
    Link {
        source: Uuid16,
        target: Uuid16,
        kind: String,
    },
    Unlink {
        source: Uuid16,
        target: Uuid16,
        kind: String,
    },
}

pub(crate) fn tag(v: &Value, path: &str) -> ApiResult<String> {
    text(v, path, MAX_TAG_BYTES)
}

pub(crate) fn relation_kind(v: &Value, path: &str) -> ApiResult<String> {
    let s = string(v, path)?;
    if !is_simple_name(s, 63) {
        return Err(invalid(
            path,
            format!("{path} must match ^[a-z][a-z0-9_]{{0,62}}$"),
        ));
    }
    Ok(s.to_string())
}

pub fn parse_annotate(v: &Value) -> ApiResult<Annotate> {
    let action = v
        .as_object()
        .and_then(|m| m.get("action"))
        .and_then(Value::as_str)
        .ok_or_else(|| invalid("action", "action must be one of tag, untag, link, unlink"))?;
    match action {
        "tag" | "untag" => {
            let m = object(
                v,
                "",
                &["action", "asset_id", "tags", "expected_annotation_revision"],
            )?;
            let asset_id = Uuid16::from_value(required(m, "", "asset_id")?, "asset_id")?;
            let expected_annotation_revision = uint(
                required(m, "", "expected_annotation_revision")?,
                "expected_annotation_revision",
                i64::MAX as u64,
            )? as i64;
            let items = array(required(m, "", "tags")?, "tags")?;
            if items.is_empty() || items.len() > MAX_TAGS_PER_CALL {
                return Err(invalid(
                    "tags",
                    format!("tags must contain 1 to {MAX_TAGS_PER_CALL} tags"),
                ));
            }
            let mut tags: Vec<String> = Vec::with_capacity(items.len());
            for (i, t) in items.iter().enumerate() {
                let t = tag(t, &format!("tags[{i}]"))?;
                if tags.contains(&t) {
                    return Err(invalid(&format!("tags[{i}]"), "tags must not repeat"));
                }
                tags.push(t);
            }
            Ok(if action == "tag" {
                Annotate::Tag {
                    asset_id,
                    tags,
                    expected_annotation_revision,
                }
            } else {
                Annotate::Untag {
                    asset_id,
                    tags,
                    expected_annotation_revision,
                }
            })
        }
        "link" | "unlink" => {
            let m = object(
                v,
                "",
                &["action", "source_evidence_id", "target_evidence_id", "kind"],
            )?;
            let source =
                Uuid16::from_value(required(m, "", "source_evidence_id")?, "source_evidence_id")?;
            let target =
                Uuid16::from_value(required(m, "", "target_evidence_id")?, "target_evidence_id")?;
            let kind = relation_kind(required(m, "", "kind")?, "kind")?;
            if source == target {
                return Err(invalid(
                    "target_evidence_id",
                    "a relation cannot link evidence to itself",
                ));
            }
            Ok(if action == "link" {
                Annotate::Link {
                    source,
                    target,
                    kind,
                }
            } else {
                Annotate::Unlink {
                    source,
                    target,
                    kind,
                }
            })
        }
        _ => Err(invalid(
            "action",
            "action must be one of tag, untag, link, unlink",
        )),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::error::SqlState;
    use serde_json::json;

    fn state<T: std::fmt::Debug>(r: ApiResult<T>) -> SqlState {
        r.expect_err("expected an error").state
    }

    const A: &str = "0b5f3c1e-8d2a-4f6b-9c3d-2e1f0a9b8c7d";

    fn stage_json(source: &str, spans: Value) -> Value {
        json!({
            "asset_id": A,
            "path": "docs/a.md",
            "source": source,
            "source_sha256": hex(&sha256(source.as_bytes())),
            "spans": spans,
            "ingestion_key": "k1",
            "expected_revision": 0
        })
    }

    #[test]
    fn corpus_names_follow_contract() {
        for ok in ["docs", "a", "a1_b", &"a".repeat(48)] {
            validate_corpus_name(ok).unwrap();
        }
        for bad in [
            "",
            "Docs",
            "1a",
            "_a",
            "a-b",
            "pg_x",
            "public",
            "evidence",
            "information_schema",
            &"a".repeat(49),
            "a\"b",
            "ä",
        ] {
            assert_eq!(
                state(validate_corpus_name(bad)),
                SqlState::InvalidParameter,
                "{bad}"
            );
        }
    }

    #[test]
    fn config_defaults_and_rejections() {
        let c = parse_config(&json!({})).unwrap();
        assert!(c.embedding.is_none());
        assert_eq!(c.text_search_config, "simple");
        assert_eq!(c.limits, Limits::DEFAULT);
        assert_eq!(c.limits.max_source_bytes, 1 << 20);
        assert_eq!(c.limits.max_response_bytes, 64 << 10);
        assert_eq!(c.limits.max_candidates_per_operator, 256);
        assert_eq!(c.limits.max_plan_nodes, 32);
        assert_eq!(c.limits.max_edges_returned, 1024);

        let c = parse_config(&json!({
            "embedding_model": "m1", "embedding_dimensions": 3,
            "text_search_config": "english", "limits": {"max_response_bytes": 2048}
        }))
        .unwrap();
        let e = c.embedding.unwrap();
        assert_eq!((e.model.as_str(), e.dimensions), ("m1", 3));
        assert_eq!(c.limits.max_response_bytes, 2048);
        assert_eq!(c.limits.max_plan_nodes, 32);

        // Explicit nulls mean no embeddings.
        assert!(
            parse_config(&json!({"embedding_model": null, "embedding_dimensions": null}))
                .unwrap()
                .embedding
                .is_none()
        );

        for bad in [
            json!([]),
            json!({"unknown": 1}),
            json!({"embedding_model": "m"}),
            json!({"embedding_dimensions": 3}),
            json!({"embedding_model": "m", "embedding_dimensions": 0}),
            json!({"embedding_model": "m", "embedding_dimensions": 2001}),
            json!({"embedding_model": "", "embedding_dimensions": 3}),
            json!({"text_search_config": ""}),
            json!({"limits": {"max_plan_nodes": 0}}),
            json!({"limits": {"bogus": 1}}),
            json!({"limits": {"max_source_bytes": 1.5}}),
            json!({"limits": {"max_source_bytes": u64::MAX}}),
        ] {
            assert_eq!(
                state(parse_config(&bad)),
                SqlState::InvalidParameter,
                "{bad}"
            );
        }
    }

    #[test]
    fn uuid_parsing_normalizes_case() {
        let u = Uuid16::parse("f", "0B5F3C1E-8D2A-4F6B-9C3D-2E1F0A9B8C7D").unwrap();
        assert_eq!(u.to_string(), A);
        for bad in [
            "",
            "0b5f3c1e8d2a4f6b9c3d2e1f0a9b8c7d",
            "0b5f3c1e-8d2a-4f6b-9c3d-2e1f0a9b8c7g",
        ] {
            assert_eq!(state(Uuid16::parse("f", bad)), SqlState::InvalidParameter);
        }
    }

    #[test]
    fn stage_accepts_exact_bytes_and_multibyte_boundaries() {
        // "é" is 2 bytes, "😀" 4 bytes, CRLF kept as is.
        let src = "é\r\n😀 x\u{0301}";
        let r = parse_stage(
            &stage_json(src, json!([{"start_byte": 0, "end_byte": 2}, {"start_byte": 4, "end_byte": 8}, {"start_byte": 0, "end_byte": src.len()}])),
            1 << 20,
        )
        .unwrap();
        assert_eq!(r.source, src);
        assert_eq!(r.span_text(r.spans[0]), "é");
        assert_eq!(r.span_text(r.spans[1]), "😀");
        assert_eq!(r.span_text(r.spans[2]), src);
        assert_eq!(r.expected_revision, 0);
        assert_eq!(r.asset_id.to_string(), A);
    }

    #[test]
    fn stage_rejects_bad_spans_and_digests() {
        let src = "é😀abc";
        let cases = [
            json!([{"start_byte": 1, "end_byte": 2}]),  // inside é
            json!([{"start_byte": 0, "end_byte": 3}]),  // inside 😀
            json!([{"start_byte": 2, "end_byte": 2}]),  // empty
            json!([{"start_byte": 3, "end_byte": 2}]),  // reversed
            json!([{"start_byte": 0, "end_byte": 10}]), // past end
            json!([{"start_byte": -1, "end_byte": 2}]),
            json!([{"start_byte": 0, "end_byte": 2}, {"start_byte": 0, "end_byte": 2}]),
            json!([{"start_byte": 0, "end_byte": 2, "x": 1}]),
            json!([[0, 2]]),
            json!({}),
        ];
        for spans in cases {
            assert_eq!(
                state(parse_stage(&stage_json(src, spans.clone()), 1 << 20)),
                SqlState::InvalidParameter,
                "{spans}"
            );
        }

        let mut bad_digest = stage_json(src, json!([]));
        bad_digest["source_sha256"] = json!(hex(&sha256(b"other")));
        let err = parse_stage(&bad_digest, 1 << 20).unwrap_err();
        assert_eq!(
            (err.state, err.reason()),
            (SqlState::InvalidParameter, "source_sha256_mismatch")
        );

        let mut upper = stage_json(src, json!([]));
        upper["source_sha256"] = json!(hex(&sha256(src.as_bytes())).to_uppercase());
        assert_eq!(
            state(parse_stage(&upper, 1 << 20)),
            SqlState::InvalidParameter
        );

        for (k, v) in [
            ("expected_revision", json!(-1)),
            ("expected_revision", json!(1.5)),
            ("expected_revision", json!("1")),
            ("expected_revision", json!(u64::MAX)),
            ("ingestion_key", json!("")),
            ("ingestion_key", json!("a\u{0001}")),
            ("path", json!("")),
            ("path", json!("a\nb")),
            ("asset_id", json!("nope")),
            ("unknown", json!(1)),
        ] {
            let mut r = stage_json(src, json!([]));
            r[k] = v.clone();
            assert_eq!(
                state(parse_stage(&r, 1 << 20)),
                SqlState::InvalidParameter,
                "{k}={v}"
            );
        }
        let mut missing = stage_json(src, json!([]));
        missing.as_object_mut().unwrap().remove("ingestion_key");
        assert_eq!(
            state(parse_stage(&missing, 1 << 20)),
            SqlState::InvalidParameter
        );
    }

    #[test]
    fn stage_enforces_source_limit() {
        let src = "abcdef";
        let err = parse_stage(&stage_json(src, json!([])), 5).unwrap_err();
        assert_eq!(
            (err.state, err.reason()),
            (SqlState::ProgramLimit, "limit_exceeded")
        );
        parse_stage(&stage_json(src, json!([])), 6).unwrap();
    }

    #[test]
    fn aggregate_span_bytes_are_capped() {
        // Overlapping spans totalling exactly 4 x 6 bytes fit; one more byte does not.
        let src = "abcdef";
        let at_cap = json!([{"start_byte": 0, "end_byte": 6}, {"start_byte": 0, "end_byte": 5},
                            {"start_byte": 1, "end_byte": 6}, {"start_byte": 0, "end_byte": 4},
                            {"start_byte": 2, "end_byte": 6}]);
        parse_stage(&stage_json(src, at_cap), 6).unwrap();
        let over = json!([{"start_byte": 0, "end_byte": 6}, {"start_byte": 0, "end_byte": 5},
                          {"start_byte": 1, "end_byte": 6}, {"start_byte": 0, "end_byte": 4},
                          {"start_byte": 2, "end_byte": 6}, {"start_byte": 1, "end_byte": 2}]);
        let e = parse_stage(&stage_json(src, over), 6).unwrap_err();
        assert_eq!(
            (e.state, e.detail["limit"].as_str()),
            (SqlState::ProgramLimit, Some("max_span_bytes_total"))
        );
    }

    #[test]
    fn embedding_values_per_call_are_capped() {
        let emb = Embedding {
            model: "m".into(),
            dimensions: 2000,
        };
        let items: Vec<Value> = (0..525).map(|i| json!({"evidence_id": format!("00000000-0000-4000-8000-{i:012}"), "vector": [1]})).collect();
        let e = parse_attach(
            &json!({"version_id": A, "model": "m", "embeddings": items}),
            &emb,
        )
        .unwrap_err();
        assert_eq!(
            (e.state, e.detail["limit"].as_str()),
            (
                SqlState::ProgramLimit,
                Some("max_embedding_values_per_call")
            )
        );
    }

    #[test]
    fn request_digest_is_canonical() {
        let src = "hello world";
        let spans = json!([{"start_byte": 0, "end_byte": 5}, {"start_byte": 6, "end_byte": 11}]);
        let a = parse_stage(&stage_json(src, spans.clone()), 1 << 20).unwrap();
        // Key order and uuid case do not matter; ingestion_key is the lookup key.
        let mut b_json = stage_json(src, spans.clone());
        b_json["asset_id"] = json!(A.to_uppercase());
        b_json["ingestion_key"] = json!("other-key");
        let b = parse_stage(&b_json, 1 << 20).unwrap();
        assert_eq!(a.request_sha256(), b.request_sha256());

        let mut c = stage_json(src, spans.clone());
        c["path"] = json!("docs/b.md");
        assert_ne!(
            a.request_sha256(),
            parse_stage(&c, 1 << 20).unwrap().request_sha256()
        );
        let mut d = stage_json(src, spans.clone());
        d["expected_revision"] = json!(1);
        assert_ne!(
            a.request_sha256(),
            parse_stage(&d, 1 << 20).unwrap().request_sha256()
        );
        let e = stage_json(
            src,
            json!([{"start_byte": 6, "end_byte": 11}, {"start_byte": 0, "end_byte": 5}]),
        );
        assert_ne!(
            a.request_sha256(),
            parse_stage(&e, 1 << 20).unwrap().request_sha256()
        );
        let f = stage_json("hello World", spans);
        assert_ne!(
            a.request_sha256(),
            parse_stage(&f, 1 << 20).unwrap().request_sha256()
        );
    }

    #[test]
    fn attach_validates_vectors() {
        let emb = Embedding {
            model: "m1".into(),
            dimensions: 3,
        };
        let ok = json!({"version_id": A, "model": "m1", "embeddings": [
            {"evidence_id": A, "vector": [1, 0.5, -2]}
        ]});
        let r = parse_attach(&ok, &emb).unwrap();
        assert_eq!(r.vectors[0].1, vec![1.0f32, 0.5, -2.0]);

        let other = "1b5f3c1e-8d2a-4f6b-9c3d-2e1f0a9b8c7d";
        for (bad, why) in [
            (
                json!({"version_id": A, "model": "m2", "embeddings": [{"evidence_id": A, "vector": [1, 0, 0]}]}),
                "model",
            ),
            (
                json!({"version_id": A, "model": "m1", "embeddings": [{"evidence_id": A, "vector": [1, 0]}]}),
                "dims",
            ),
            (
                json!({"version_id": A, "model": "m1", "embeddings": [{"evidence_id": A, "vector": [0, 0, 0]}]}),
                "zero",
            ),
            (
                json!({"version_id": A, "model": "m1", "embeddings": [{"evidence_id": A, "vector": [1e39, 0, 0]}]}),
                "overflow",
            ),
            (
                json!({"version_id": A, "model": "m1", "embeddings": [{"evidence_id": A, "vector": [1, "x", 0]}]}),
                "type",
            ),
            (
                json!({"version_id": A, "model": "m1", "embeddings": []}),
                "empty",
            ),
            (
                json!({"version_id": A, "model": "m1", "embeddings": [
                {"evidence_id": A, "vector": [1, 0, 0]}, {"evidence_id": A, "vector": [1, 0, 0]}]}),
                "dup",
            ),
            (
                json!({"version_id": A, "model": "m1", "embeddings": [{"evidence_id": other, "vector": [1, 0, 0], "x": 1}]}),
                "unknown",
            ),
        ] {
            assert_eq!(
                state(parse_attach(&bad, &emb)),
                SqlState::InvalidParameter,
                "{why}"
            );
        }
    }

    #[test]
    fn small_requests() {
        assert_eq!(
            parse_publish(&json!({"version_id": A}))
                .unwrap()
                .version_id
                .to_string(),
            A
        );
        assert_eq!(
            state(parse_publish(&json!({"version_id": A, "x": 1}))),
            SqlState::InvalidParameter
        );

        let r = parse_retire(&json!({"asset_id": A, "expected_revision": 3})).unwrap();
        assert_eq!(r.expected_revision, 3);
        assert_eq!(
            state(parse_retire(&json!({"asset_id": A}))),
            SqlState::InvalidParameter
        );

        let p = parse_purge(&json!({"version_id": A, "reason": "legal hold ended"})).unwrap();
        assert_eq!(p.reason, "legal hold ended");
        assert_eq!(
            state(parse_purge(&json!({"version_id": A, "reason": ""}))),
            SqlState::InvalidParameter
        );
        assert_eq!(
            state(parse_purge(&json!({"version_id": A}))),
            SqlState::InvalidParameter
        );
    }

    #[test]
    fn annotate_is_discriminated() {
        let other = "1b5f3c1e-8d2a-4f6b-9c3d-2e1f0a9b8c7d";
        match parse_annotate(&json!({"action": "tag", "asset_id": A, "tags": ["a", "b c"], "expected_annotation_revision": 0})).unwrap() {
            Annotate::Tag { tags, expected_annotation_revision, .. } => {
                assert_eq!(tags, vec!["a".to_string(), "b c".to_string()]);
                assert_eq!(expected_annotation_revision, 0);
            }
            other => panic!("{other:?}"),
        }
        assert!(matches!(
            parse_annotate(&json!({"action": "untag", "asset_id": A, "tags": ["a"], "expected_annotation_revision": 2})).unwrap(),
            Annotate::Untag { .. }
        ));
        assert!(matches!(
            parse_annotate(&json!({"action": "link", "source_evidence_id": A, "target_evidence_id": other, "kind": "supports"})).unwrap(),
            Annotate::Link { .. }
        ));
        assert!(matches!(
            parse_annotate(&json!({"action": "unlink", "source_evidence_id": A, "target_evidence_id": other, "kind": "supports"})).unwrap(),
            Annotate::Unlink { .. }
        ));
        for bad in [
            json!({"action": "rename"}),
            json!({"asset_id": A}),
            json!({"action": "tag", "asset_id": A, "tags": [], "expected_annotation_revision": 0}),
            json!({"action": "tag", "asset_id": A, "tags": ["a", "a"], "expected_annotation_revision": 0}),
            json!({"action": "tag", "asset_id": A, "tags": [""], "expected_annotation_revision": 0}),
            json!({"action": "tag", "asset_id": A, "tags": ["a\tb"], "expected_annotation_revision": 0}),
            json!({"action": "tag", "asset_id": A, "tags": ["a"]}),
            json!({"action": "tag", "asset_id": A, "tags": ["a"], "expected_annotation_revision": 0, "kind": "x"}),
            json!({"action": "link", "source_evidence_id": A, "target_evidence_id": A, "kind": "supports"}),
            json!({"action": "link", "source_evidence_id": A, "target_evidence_id": other, "kind": "Supports"}),
            json!({"action": "link", "source_evidence_id": A, "target_evidence_id": other}),
        ] {
            assert_eq!(
                state(parse_annotate(&bad)),
                SqlState::InvalidParameter,
                "{bad}"
            );
        }
    }
}
