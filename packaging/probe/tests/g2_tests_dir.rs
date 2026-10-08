//! G2 experiment: a `#[pg_test]` defined under tests/ rather than src/.
//! Mirrors the src/ layout as closely as an integration-test crate allows.

#[pgrx::pg_schema]
mod tests {
    use pgrx::prelude::*;

    #[pg_test]
    fn tests_dir_pg_test_runs_in_backend() {
        let version = Spi::get_one::<String>("SELECT current_setting('server_version')")
            .expect("SPI failed")
            .expect("server_version is never NULL");
        assert!(version.starts_with("18."));
    }
}

pub mod pg_test {
    pub fn setup(_options: Vec<&str>) {}

    #[must_use]
    pub fn postgresql_conf_options() -> Vec<&'static str> {
        vec![]
    }
}
