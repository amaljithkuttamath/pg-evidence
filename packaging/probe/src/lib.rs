//! Build-environment probe for gates G1 and G2. Not a pg-evidence product API.

use pgrx::prelude::*;

::pgrx::pg_module_magic!(name, version);

/// Server version as seen from inside the loaded extension.
#[pg_extern]
fn probe_server_version() -> String {
    Spi::get_one::<String>("SELECT current_setting('server_version')")
        .expect("SPI failed")
        .expect("server_version is never NULL")
}

/// Cosine distance computed by pgvector through SPI, proving both extensions
/// work in one backend.
#[pg_extern]
fn probe_vector_cosine_distance() -> f64 {
    Spi::get_one::<f64>("SELECT '[1,0,0]'::vector <=> '[0,1,0]'::vector")
        .expect("SPI failed; is the vector extension installed?")
        .expect("distance is never NULL")
}

#[cfg(any(test, feature = "pg_test"))]
#[pg_schema]
mod tests {
    use pgrx::prelude::*;

    // G2: a database test defined under src/.
    #[pg_test]
    fn src_pg_test_runs_in_backend() {
        assert!(crate::probe_server_version().starts_with("18."));
    }

    #[pg_test]
    fn src_pg_test_with_pgvector() {
        Spi::run("CREATE EXTENSION IF NOT EXISTS vector").expect("create vector");
        assert_eq!(crate::probe_vector_cosine_distance(), 1.0);
    }
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
