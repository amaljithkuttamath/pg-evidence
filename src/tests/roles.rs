// G10: column grants allow the API path and deny direct edits of retained
// bytes. The statements mirror sql/grants.sql and docs/operations.md.

fn create_roles_and_grant(corpus: &str) {
    install_error_probe();
    for stmt in [
        "CREATE ROLE pgev_t_reader NOLOGIN",
        "CREATE ROLE pgev_t_writer NOLOGIN",
        "CREATE ROLE pgev_t_purger NOLOGIN",
        "GRANT pgev_t_reader TO pgev_t_writer",
        "GRANT pgev_t_writer TO pgev_t_purger",
        "GRANT USAGE ON SCHEMA evidence TO pgev_t_reader",
    ] {
        exec(stmt);
    }
    let c = corpus;
    for stmt in [
        format!("GRANT USAGE ON SCHEMA {c} TO pgev_t_reader"),
        format!("GRANT SELECT ON ALL TABLES IN SCHEMA {c} TO pgev_t_reader"),
        format!("GRANT INSERT ON {c}.assets, {c}.versions, {c}.spans, {c}.publications, {c}.tags, {c}.relations TO pgev_t_writer"),
        format!("GRANT UPDATE (current_version_id, current_path, content_revision, annotation_revision, retired_at) ON {c}.assets TO pgev_t_writer"),
        format!("GRANT DELETE ON {c}.tags, {c}.relations TO pgev_t_writer"),
        format!("GRANT UPDATE (source) ON {c}.versions TO pgev_t_purger"),
        format!("GRANT UPDATE (text) ON {c}.spans TO pgev_t_purger"),
        format!("GRANT INSERT ON {c}.tombstones TO pgev_t_purger"),
    ] {
        exec(&stmt);
    }
}

#[pg_test]
fn writer_cannot_alter_retained_bytes() {
    init("docs", json!({}));
    with_timeout();
    create_roles_and_grant("docs");

    exec("SET LOCAL ROLE pgev_t_writer");
    let a = stage(
        "docs",
        ASSET_A,
        "a.md",
        "retained bytes",
        &[(0, 8)],
        "k1",
        0,
    );
    publish("docs", &version_id(&a));
    let b = stage("docs", ASSET_A, "a.md", "newer bytes", &[(0, 5)], "k2", 1);
    publish("docs", &version_id(&b));
    call(
        "annotate",
        "docs",
        json!({"action": "tag", "asset_id": ASSET_A, "tags": ["t"], "expected_annotation_revision": 0}),
    );
    call(
        "annotate",
        "docs",
        json!({"action": "link", "source_evidence_id": evidence_id(&b, 0),
                                    "target_evidence_id": evidence_id(&a, 0), "kind": "replaces"}),
    );
    let vid = lit(&version_id(&a));
    for (sql, expected) in [
        (format!("UPDATE docs.versions SET source = 'x' WHERE version_id = {vid}::uuid"), "42501"),
        (format!("UPDATE docs.versions SET source_sha256 = '\\x00' WHERE version_id = {vid}::uuid"), "42501"),
        ("UPDATE docs.versions SET path = 'moved.md'".to_string(), "42501"),
        ("DELETE FROM docs.versions".to_string(), "42501"),
        ("UPDATE docs.spans SET text = 'x'".to_string(), "42501"),
        ("DELETE FROM docs.spans".to_string(), "42501"),
        ("UPDATE docs.publications SET revision = 9".to_string(), "42501"),
        ("DELETE FROM docs.publications".to_string(), "42501"),
        ("INSERT INTO docs.tombstones (version_id, reason) SELECT version_id, 'x' FROM docs.versions".to_string(), "42501"),
        ("UPDATE docs.collection_config SET max_source_bytes = 1".to_string(), "42501"),
        ("TRUNCATE docs.spans".to_string(), "42501"),
        // FOR UPDATE is satisfied by the assets column grant.
        ("SELECT * FROM docs.assets FOR UPDATE".to_string(), "00000"),
        // A forged version whose digest does not match its own source.
        (format!("INSERT INTO docs.versions (version_id, asset_id, path, source, source_sha256, byte_length, \
                 ingestion_key, request_sha256, base_revision) VALUES (gen_random_uuid(), {}::uuid, 'f.md', 'forged', \
                 sha256('other'::bytea), 6, 'forged', '\\x00', 0)", lit(ASSET_A)), "23514"),
    ] {
        assert_eq!(sqlstate(&sql), expected, "{sql}");
    }
    let (state, reason) = call_err(
        "purge",
        "docs",
        json!({"version_id": version_id(&a), "reason": "no"}),
    );
    assert_eq!(
        (state.as_str(), reason.as_str()),
        ("42501", "purger_required")
    );

    exec("RESET ROLE");
    exec("SET LOCAL ROLE pgev_t_purger");
    let p = call(
        "purge",
        "docs",
        json!({"version_id": version_id(&a), "reason": "allowed"}),
    );
    assert_eq!(p["status"], "purged");
    assert_eq!(
        sqlstate("UPDATE docs.versions SET path = 'moved.md'"),
        "42501"
    );

    exec("RESET ROLE");
    exec("SET LOCAL ROLE pgev_t_reader");
    let q = query(
        "docs",
        json!({"nodes": [{"id": "a", "op": "literal", "text": "newer"}], "output": "a"}),
    );
    assert_eq!(q["results"].as_array().map(Vec::len), Some(1));
    assert_eq!(resolve("docs", &evidence_id(&b, 0))["text"], "newer");
    assert_eq!(
        call_err(
            "stage_version",
            "docs",
            stage_req(ASSET_B, "b.md", "x", &[], "k3", 0)
        )
        .0,
        "42501"
    );
    assert_eq!(
        call_err(
            "publish_version",
            "docs",
            json!({"version_id": version_id(&b)})
        )
        .0,
        "42501"
    );
    assert_eq!(call_err("annotate", "docs", json!({"action": "untag", "asset_id": ASSET_A, "tags": ["t"], "expected_annotation_revision": 1})).0, "42501");
    exec("RESET ROLE");
}

#[pg_test]
fn role_without_corpus_usage_sees_nothing() {
    init("docs", json!({}));
    with_timeout();
    stage("docs", ASSET_A, "a.md", "hidden", &[(0, 6)], "k1", 0);
    install_error_probe();
    exec("CREATE ROLE pgev_t_outsider NOLOGIN");
    exec("GRANT USAGE ON SCHEMA evidence TO pgev_t_outsider");
    exec("SET LOCAL ROLE pgev_t_outsider");
    assert_eq!(sqlstate("SELECT evidence.query('docs', '{\"nodes\": [{\"id\": \"a\", \"op\": \"literal\", \"text\": \"hidden\"}], \"output\": \"a\"}')"), "42501");
    assert_eq!(
        sqlstate("SELECT evidence.resolve('docs', gen_random_uuid())"),
        "42501"
    );
    exec("RESET ROLE");
}
