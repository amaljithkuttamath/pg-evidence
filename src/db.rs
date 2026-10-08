//! Backend glue: the jsonb request and json response types, parameterized SPI
//! helpers, collection loading and conversion of `ApiError` into a PostgreSQL
//! ERROR with the contract's SQLSTATE and a JSON detail.

use crate::error::{internal, invalid, precondition, ApiError, ApiResult, SqlState};
use crate::model::{validate_corpus_name, CollectionConfig, Embedding, Limits, SCHEMA_VERSION};
use crate::plan::quote_ident;
use pgrx::callconv::{Arg, ArgAbi, BoxRet, FcInfo};
use pgrx::datum::{Datum, DatumWithOid};
use pgrx::nullable::Nullable;
use pgrx::prelude::*;
use pgrx::{pg_sys, FromDatum, IntoDatum, Json};
use serde_json::Value;
use std::ffi::CStr;

/// A `jsonb` argument. Unlike `pgrx::JsonB`, a document serde_json cannot
/// represent (nesting deeper than 128, numbers out of f64 range) becomes a
/// 22023 instead of an internal panic.
pub struct Request(Result<Value, String>);

impl Request {
    pub fn value(&self) -> ApiResult<&Value> {
        self.0
            .as_ref()
            .map_err(|e| invalid("request", format!("request cannot be represented: {e}")))
    }
}

impl FromDatum for Request {
    unsafe fn from_polymorphic_datum(
        datum: pg_sys::Datum,
        is_null: bool,
        _: pg_sys::Oid,
    ) -> Option<Self> {
        if is_null {
            return None;
        }
        // Validate the server encoding before decoding jsonb_out as UTF-8.
        if let Err(error) = require_utf8() {
            raise(error);
        }
        unsafe {
            let varlena = datum.cast_mut_ptr();
            let detoasted = pg_sys::pg_detoast_datum_packed(varlena);
            let text =
                pgrx::direct_function_call::<&CStr>(pg_sys::jsonb_out, &[Some(detoasted.into())])
                    .expect("jsonb_out never returns NULL for a non-null jsonb");
            let parsed = match text.to_str() {
                Ok(s) => serde_json::from_str(s).map_err(|e| e.to_string()),
                Err(e) => Err(e.to_string()),
            };
            Some(Request(parsed))
        }
    }
}

unsafe impl<'fcx> ArgAbi<'fcx> for Request {
    unsafe fn unbox_arg_unchecked(arg: Arg<'_, 'fcx>) -> Self {
        let index = arg.index();
        unsafe {
            arg.unbox_arg_using_from_datum()
                .unwrap_or_else(|| panic!("argument {index} must not be null"))
        }
    }

    unsafe fn unbox_nullable_arg(arg: Arg<'_, 'fcx>) -> Nullable<Self> {
        unsafe { arg.unbox_arg_using_from_datum().into() }
    }
}

pgrx::impl_sql_translatable!(Request, "jsonb");

/// A `json` result whose bytes are exactly the rendered string, so the
/// response budget is measured on what the client receives.
pub struct JsonText(pub String);

impl IntoDatum for JsonText {
    fn into_datum(self) -> Option<pg_sys::Datum> {
        self.0.into_datum()
    }

    fn type_oid() -> pg_sys::Oid {
        pg_sys::JSONOID
    }
}

unsafe impl BoxRet for JsonText {
    unsafe fn box_into<'fcx>(self, fcinfo: &mut FcInfo<'fcx>) -> Datum<'fcx> {
        unsafe { fcinfo.return_optional_datum(self.into_datum()) }
    }
}

pgrx::impl_sql_translatable!(JsonText, "json");

pub fn raise(e: ApiError) -> ! {
    let code = match e.state {
        SqlState::InvalidParameter => PgSqlErrorCode::ERRCODE_INVALID_PARAMETER_VALUE,
        SqlState::UniqueViolation => PgSqlErrorCode::ERRCODE_UNIQUE_VIOLATION,
        SqlState::ProgramLimit => PgSqlErrorCode::ERRCODE_PROGRAM_LIMIT_EXCEEDED,
        SqlState::Prerequisite => PgSqlErrorCode::ERRCODE_OBJECT_NOT_IN_PREREQUISITE_STATE,
        SqlState::DataCorrupted => PgSqlErrorCode::ERRCODE_DATA_CORRUPTED,
        SqlState::InsufficientPrivilege => PgSqlErrorCode::ERRCODE_INSUFFICIENT_PRIVILEGE,
        SqlState::Internal => PgSqlErrorCode::ERRCODE_INTERNAL_ERROR,
    };
    let detail = e.detail_text();
    ereport!(ERROR, code, e.message, detail);
}

/// Runs an API body and raises its error, if any.
pub fn run<T>(f: impl FnOnce() -> ApiResult<T>) -> T {
    match f() {
        Ok(v) => v,
        Err(e) => raise(e),
    }
}

fn spi_failed(e: pgrx::spi::SpiError) -> ApiError {
    internal(format!("SPI failed: {e}"))
}

/// SPI parameter values. UUIDs and digests travel as text and are cast in SQL.
pub enum P {
    Text(String),
    OptText(Option<String>),
    Int4(i32),
    OptInt4(Option<i32>),
    Int8(i64),
    Int4Array(Vec<i32>),
    TextArray(Vec<String>),
    Float4Array(Vec<f32>),
}

impl P {
    pub fn uuid(u: impl ToString) -> P {
        P::Text(u.to_string())
    }
}

fn datum(p: P) -> DatumWithOid<'static> {
    match p {
        P::Text(s) | P::OptText(Some(s)) => s.into(),
        P::OptText(None) => DatumWithOid::null::<String>(),
        P::Int4(n) | P::OptInt4(Some(n)) => n.into(),
        P::OptInt4(None) => DatumWithOid::null::<i32>(),
        P::Int8(n) => n.into(),
        P::Int4Array(v) => v.into(),
        P::TextArray(v) => v.into(),
        P::Float4Array(v) => v.into(),
    }
}

fn args(params: Vec<P>) -> Vec<DatumWithOid<'static>> {
    params.into_iter().map(datum).collect()
}

/// Executes a writable statement whose first column of its first row, if
/// any, is `json`. Statements without rows return `Ok(None)`.
pub fn write_json(sql: &str, params: Vec<P>) -> ApiResult<Option<Value>> {
    let args = args(params);
    Spi::connect_mut(|c| -> pgrx::spi::Result<Option<Json>> {
        let table = c.update(sql, None, &args)?;
        if table.is_empty() {
            return Ok(None);
        }
        table.first().get_one::<Json>()
    })
    .map(|j| j.map(|j| j.0))
    .map_err(spi_failed)
}

/// Executes a writable statement and returns the number of rows processed.
pub fn write_exec(sql: &str, params: Vec<P>) -> ApiResult<usize> {
    let args = args(params);
    Spi::connect_mut(|c| c.update(sql, None, &args).map(|t| t.len())).map_err(spi_failed)
}

/// Read-only counterpart of `write_json` for `STABLE` functions.
pub fn read_json(sql: &str, params: Vec<P>) -> ApiResult<Option<Value>> {
    let args = args(params);
    Spi::connect(|c| -> pgrx::spi::Result<Option<Json>> {
        let table = c.select(sql, None, &args)?;
        if table.is_empty() {
            return Ok(None);
        }
        table.first().get_one::<Json>()
    })
    .map(|j| j.map(|j| j.0))
    .map_err(spi_failed)
}

/// Read-only statement returning one row of three `json` columns.
pub fn read_three_json(sql: &str, params: Vec<P>) -> ApiResult<(Value, Value, Value)> {
    let args = args(params);
    let (a, b, c) = Spi::connect(
        |c| -> pgrx::spi::Result<(Option<Json>, Option<Json>, Option<Json>)> {
            c.select(sql, None, &args)?
                .first()
                .get_three::<Json, Json, Json>()
        },
    )
    .map_err(spi_failed)?;
    let v = |j: Option<Json>| j.map(|j| j.0).unwrap_or(Value::Null);
    Ok((v(a), v(b), v(c)))
}

/// Every function refuses a non-UTF8 database (55000).
pub fn require_utf8() -> ApiResult<()> {
    let name = unsafe { CStr::from_ptr(pg_sys::GetDatabaseEncodingName()) };
    if name.to_bytes() != b"UTF8" {
        return Err(precondition(
            "non_utf8_database",
            format!(
                "pg_evidence requires a UTF8 database; this one is {}",
                name.to_string_lossy()
            ),
        ));
    }
    Ok(())
}

pub struct Collection {
    pub config: CollectionConfig,
    /// pgvector's schema, read from pg_extension at call time.
    pub vector_schema: Option<String>,
    pub statement_timeout: String,
}

impl Collection {
    /// Embedding configuration plus pgvector's schema, for operations that need both.
    pub fn vectors(&self) -> ApiResult<(&Embedding, &str)> {
        let emb = self.config.embedding.as_ref().ok_or_else(|| {
            ApiError::new(
                SqlState::InvalidParameter,
                "embeddings_not_configured",
                "this collection has no embedding configuration",
            )
        })?;
        let vs = self.vector_schema.as_deref().ok_or_else(|| {
            precondition("pgvector_missing", "the vector extension is not installed")
        })?;
        Ok((emb, vs))
    }

    pub fn semantic_readiness(&self) -> &'static str {
        match (&self.config.embedding, &self.vector_schema) {
            (None, _) => "not_configured",
            (Some(_), None) => "unavailable",
            (Some(_), Some(_)) => "ready",
        }
    }
}

pub fn vector_schema_sql() -> &'static str {
    "SELECT pg_catalog.to_json(n.nspname::pg_catalog.text) FROM pg_catalog.pg_extension e \
     JOIN pg_catalog.pg_namespace n ON n.oid OPERATOR(pg_catalog.=) e.extnamespace \
     WHERE e.extname OPERATOR(pg_catalog.=) 'vector'"
}

/// Loads collection_config (one statement). `read_only` selects SPI mode for
/// `STABLE` functions. Unknown corpora surface PostgreSQL's own 42P01/3F000.
pub fn load_collection(corpus: &str, read_only: bool) -> ApiResult<Collection> {
    require_utf8()?;
    validate_corpus_name(corpus)?;
    let c = quote_ident(corpus);
    let sql = format!(
        "SELECT pg_catalog.json_build_array(\
         (SELECT pg_catalog.json_agg(pg_catalog.json_build_array(k.schema_version, k.embedding_model, \
         k.embedding_dimensions, k.text_search_config, k.max_source_bytes, k.max_response_bytes, \
         k.max_candidates_per_operator, k.max_plan_nodes, k.max_edges_returned)) FROM {c}.\"collection_config\" k), \
         ({vs}), pg_catalog.current_setting('statement_timeout'))",
        vs = vector_schema_sql()
    );
    let v = if read_only {
        read_json(&sql, vec![])?
    } else {
        write_json(&sql, vec![])?
    }
    .ok_or_else(|| internal("collection_config query returned no row"))?;
    let bad = || {
        precondition(
            "invalid_collection_config",
            format!("{corpus}.collection_config must hold exactly one valid row"),
        )
    };
    let rows = v[0].as_array().filter(|r| r.len() == 1).ok_or_else(bad)?;
    let row = rows[0]
        .as_array()
        .filter(|r| r.len() == 9)
        .ok_or_else(bad)?;
    let version = row[0].as_i64().ok_or_else(bad)?;
    if version != SCHEMA_VERSION as i64 {
        return Err(precondition(
            "unsupported_schema_version",
            format!("corpus {corpus} has schema_version {version}; this pg_evidence supports {SCHEMA_VERSION}"),
        )
        .with("schema_version", version)
        .with("hint", "migrate the corpus with the pg_evidence release that introduced its schema version"));
    }
    let limit = |i: usize| -> ApiResult<u32> {
        row[i]
            .as_u64()
            .filter(|n| *n >= 1 && *n <= u32::MAX as u64)
            .map(|n| n as u32)
            .ok_or_else(bad)
    };
    let embedding = match (&row[1], &row[2]) {
        (Value::Null, Value::Null) => None,
        (Value::String(model), dims) => Some(Embedding {
            model: model.clone(),
            dimensions: dims
                .as_u64()
                .filter(|d| (1..=2000).contains(d))
                .ok_or_else(bad)? as u32,
        }),
        _ => return Err(bad()),
    };
    Ok(Collection {
        config: CollectionConfig {
            embedding,
            text_search_config: row[3].as_str().ok_or_else(bad)?.to_string(),
            limits: Limits {
                max_source_bytes: limit(4)?,
                max_response_bytes: limit(5)?,
                max_candidates_per_operator: limit(6)?,
                max_plan_nodes: limit(7)?,
                max_edges_returned: limit(8)?,
            },
        },
        vector_schema: v[1].as_str().map(str::to_string),
        statement_timeout: v[2].as_str().unwrap_or("0").to_string(),
    })
}
