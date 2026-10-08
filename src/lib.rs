//! pg-evidence v0.1: versioned evidence and bounded retrieval for PostgreSQL.
//!
//! The SQL API lives in schema `evidence` (docs/api.md). Every function is
//! SECURITY INVOKER, STRICT and PARALLEL UNSAFE and runs with
//! `search_path = pg_catalog, pg_temp`; ordinary grants on corpus tables are
//! the authorization boundary (docs/operations.md). No model inference runs in
//! the backend: embeddings arrive as vectors computed by the client.

use pgrx::prelude::*;

mod db;
mod ddl;
mod error;
mod model;
mod ops;
mod plan;
mod render;
mod retrieve;

use db::{run, JsonText, Request};

::pgrx::pg_module_magic!(name, version);

/// Creates the corpus schema and its tables, owned by the caller.
#[pg_extern(volatile, parallel_unsafe, security_invoker, strict)]
#[search_path(pg_catalog, pg_temp)]
fn init_collection(corpus: &str, config: Request) {
    run(|| ops::init_collection(corpus, config.value()?))
}

/// Stages an immutable version and its spans; idempotent per ingestion_key.
#[pg_extern(volatile, parallel_unsafe, security_invoker, strict)]
#[search_path(pg_catalog, pg_temp)]
fn stage_version(corpus: &str, request: Request) -> JsonText {
    JsonText(run(|| ops::stage_version(corpus, request.value()?)))
}

/// Attaches client-computed embeddings to the spans of a staged version.
#[pg_extern(volatile, parallel_unsafe, security_invoker, strict)]
#[search_path(pg_catalog, pg_temp)]
fn attach_embeddings(corpus: &str, request: Request) {
    run(|| ops::attach_embeddings(corpus, request.value()?))
}

/// Makes a staged version current if its base revision is still current.
#[pg_extern(volatile, parallel_unsafe, security_invoker, strict)]
#[search_path(pg_catalog, pg_temp)]
fn publish_version(corpus: &str, request: Request) -> JsonText {
    JsonText(run(|| ops::publish_version(corpus, request.value()?)))
}

/// Removes an asset from current retrieval; its versions stay resolvable.
#[pg_extern(volatile, parallel_unsafe, security_invoker, strict)]
#[search_path(pg_catalog, pg_temp)]
fn retire(corpus: &str, request: Request) -> JsonText {
    JsonText(run(|| ops::retire(corpus, request.value()?)))
}

/// tag / untag an asset, link / unlink two evidence IDs.
#[pg_extern(volatile, parallel_unsafe, security_invoker, strict)]
#[search_path(pg_catalog, pg_temp)]
fn annotate(corpus: &str, request: Request) -> JsonText {
    JsonText(run(|| ops::annotate(corpus, request.value()?)))
}

/// Logically removes a non-current version's bytes, keeping IDs and a tombstone.
#[pg_extern(volatile, parallel_unsafe, security_invoker, strict)]
#[search_path(pg_catalog, pg_temp)]
fn purge(corpus: &str, request: Request) -> JsonText {
    JsonText(run(|| ops::purge(corpus, request.value()?)))
}

/// Runs a bounded retrieval plan as one SQL statement.
#[pg_extern(stable, parallel_unsafe, security_invoker, strict)]
#[search_path(pg_catalog, pg_temp)]
fn query(corpus: &str, request: Request) -> JsonText {
    JsonText(run(|| retrieve::query(corpus, request.value()?)))
}

/// Resolves an evidence ID to its exact retained, verified bytes or status.
#[pg_extern(stable, parallel_unsafe, security_invoker, strict)]
#[search_path(pg_catalog, pg_temp)]
fn resolve(corpus: &str, evidence_id: pgrx::Uuid) -> JsonText {
    JsonText(run(|| retrieve::resolve(corpus, &evidence_id.to_string())))
}

/// Backend tests: compiled into the extension crate (gate G2) and run by
/// `cargo pgrx test pg18`. Files are included so every test lives in the
/// `tests` schema the pgrx runner calls.
#[cfg(any(test, feature = "pg_test"))]
#[pg_schema]
mod tests {
    use pgrx::prelude::*;
    use serde_json::{json, Value};

    include!("tests/support.rs");
    include!("tests/catalog.rs");
    include!("tests/lifecycle.rs");
    include!("tests/annotate.rs");
    include!("tests/retrieval.rs");
    include!("tests/roles.rs");
}

/// Required by `cargo pgrx test`; must be visible at the crate root.
#[cfg(test)]
pub mod pg_test {
    pub fn setup(_options: Vec<&str>) {}

    #[must_use]
    pub fn postgresql_conf_options() -> Vec<&'static str> {
        vec![]
    }
}
