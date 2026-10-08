//! API errors and the SQLSTATEs fixed by docs/design.md. Pure Rust: the backend
//! layer (`db::raise`) maps each state to its SQLSTATE. 42P06 (corpus exists)
//! and 42501 from grants are raised by PostgreSQL itself.

use serde_json::{json, Value};

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum SqlState {
    /// 22023: request failed extension validation.
    InvalidParameter,
    /// 23505: ingestion key reuse, conflicting embeddings.
    UniqueViolation,
    /// 54000: configured size limit exceeded, or the envelope cannot fit.
    ProgramLimit,
    /// 55000: precondition failed; the detail carries a machine-readable reason.
    Prerequisite,
    /// XX001: retained bytes fail digest or span verification.
    DataCorrupted,
    /// 42501: the caller lacks a privilege the operation requires.
    InsufficientPrivilege,
    /// XX000: an internal invariant failed (a bug, never a request error).
    Internal,
}

/// `detail` is always a JSON object with at least a `reason` key.
#[derive(Debug, Clone, PartialEq)]
pub struct ApiError {
    pub state: SqlState,
    pub message: String,
    pub detail: Value,
}

pub type ApiResult<T> = Result<T, ApiError>;

impl ApiError {
    pub fn new(state: SqlState, reason: &str, message: impl Into<String>) -> Self {
        ApiError {
            state,
            message: message.into(),
            detail: json!({ "reason": reason }),
        }
    }

    /// Adds a key to the detail object.
    pub fn with(mut self, key: &str, value: impl Into<Value>) -> Self {
        if let Value::Object(map) = &mut self.detail {
            map.insert(key.to_string(), value.into());
        }
        self
    }

    #[cfg(test)]
    pub fn reason(&self) -> &str {
        self.detail
            .get("reason")
            .and_then(Value::as_str)
            .unwrap_or("")
    }

    pub fn detail_text(&self) -> String {
        self.detail.to_string()
    }
}

/// 22023 for a malformed request field. `field` is a JSON path such as `spans[2].end_byte`.
pub fn invalid(field: &str, message: impl Into<String>) -> ApiError {
    ApiError::new(SqlState::InvalidParameter, "invalid_request", message).with("field", field)
}

/// 54000 for a request exceeding a configured or fixed limit.
pub fn limit_exceeded(limit: &str, max: u64, message: impl Into<String>) -> ApiError {
    ApiError::new(SqlState::ProgramLimit, "limit_exceeded", message)
        .with("limit", limit)
        .with("max", max)
}

/// 55000 with a reason such as `revision_conflict`.
pub fn precondition(reason: &str, message: impl Into<String>) -> ApiError {
    ApiError::new(SqlState::Prerequisite, reason, message)
}

pub fn unique(reason: &str, message: impl Into<String>) -> ApiError {
    ApiError::new(SqlState::UniqueViolation, reason, message)
}

pub fn corrupted(reason: &str, message: impl Into<String>) -> ApiError {
    ApiError::new(SqlState::DataCorrupted, reason, message)
}

pub fn internal(message: impl Into<String>) -> ApiError {
    ApiError::new(SqlState::Internal, "internal_error", message)
}
