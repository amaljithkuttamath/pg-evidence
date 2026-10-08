// Version lifecycle, exact bytes, retry and revision rules, purge, corruption.

#[pg_test]
fn stage_publish_resolve_exact_unicode_bytes() {
    init("docs", json!({}));
    // Multibyte, combining character, CRLF and a BOM are stored unnormalized.
    let src = "\u{feff}Caf\u{e9} cafe\u{301}\r\n\u{1f600} end";
    let e_start = src.find("cafe").unwrap();
    let e_end = e_start + "cafe\u{301}".len();
    let staged = stage(
        "docs",
        ASSET_A,
        "notes/caf\u{e9}.md",
        src,
        &[(0, src.len()), (e_start, e_end)],
        "k-a",
        0,
    );
    assert_eq!(staged["status"], "staged");
    assert_eq!(staged["replayed"], false);
    assert_eq!(staged["byte_length"], src.len());
    assert_eq!(staged["source_sha256"], sha(src));
    assert_eq!(staged["spans"][1]["start_byte"], e_start);

    // Staged evidence resolves with status staged and is not retrievable.
    assert_eq!(
        resolve("docs", &evidence_id(&staged, 1))["status"],
        "staged"
    );

    let published = publish("docs", &version_id(&staged));
    assert_eq!(
        (
            published["revision"].as_i64(),
            published["current"].as_bool()
        ),
        (Some(1), Some(true))
    );

    let r = resolve("docs", &evidence_id(&staged, 1));
    assert_eq!(r["status"], "current");
    assert_eq!(r["text"], "cafe\u{301}");
    assert_eq!(r["verified"], true);
    assert_eq!(r["published_revision"], 1);
    let whole = resolve("docs", &evidence_id(&staged, 0));
    assert_eq!(whole["text"].as_str(), Some(src));
    // Stored bytes equal the received bytes.
    assert_eq!(
        text(&format!("SELECT encode(sha256(convert_to(source, 'UTF8')), 'hex') FROM docs.versions WHERE version_id = {}::uuid",
            lit(&version_id(&staged)))).as_deref(),
        Some(sha(src).as_str())
    );
    assert_eq!(
        resolve("docs", "00000000-0000-4000-8000-000000000000"),
        json!({"status": "not_found"})
    );
}

#[pg_test]
fn publishing_b_keeps_a_citation_resolvable() {
    init("docs", json!({}));
    let a = stage(
        "docs",
        ASSET_A,
        "a.md",
        "alpha original text",
        &[(0, 5)],
        "k-a",
        0,
    );
    publish("docs", &version_id(&a));
    let b = stage(
        "docs",
        ASSET_A,
        "a.md",
        "alpha edited text",
        &[(0, 5), (6, 12)],
        "k-b",
        1,
    );
    let pb = publish("docs", &version_id(&b));
    assert_eq!(pb["revision"], 2);

    let old = resolve("docs", &evidence_id(&a, 0));
    assert_eq!(
        (old["status"].as_str(), old["text"].as_str()),
        (Some("historical"), Some("alpha"))
    );
    assert_eq!(old["version_id"], a["version_id"]);
    assert_eq!(resolve("docs", &evidence_id(&b, 1))["status"], "current");
    assert_eq!(
        int(&format!(
            "SELECT content_revision FROM docs.assets WHERE asset_id = {}::uuid",
            lit(ASSET_A)
        )),
        2
    );
    // Evidence IDs are never reused.
    assert_ne!(evidence_id(&a, 0), evidence_id(&b, 0));
}

#[pg_test]
fn retry_returns_stored_ids_and_key_reuse_conflicts() {
    init("docs", json!({}));
    let first = stage(
        "docs",
        ASSET_A,
        "a.md",
        "hello world",
        &[(0, 5), (6, 11)],
        "k1",
        0,
    );
    let again = stage(
        "docs",
        ASSET_A,
        "a.md",
        "hello world",
        &[(0, 5), (6, 11)],
        "k1",
        0,
    );
    assert_eq!(again["replayed"], true);
    assert_eq!(again["version_id"], first["version_id"]);
    assert_eq!(again["spans"], first["spans"]);
    assert_eq!(int("SELECT count(*) FROM docs.versions"), 1);
    assert_eq!(int("SELECT count(*) FROM docs.spans"), 2);

    // A lost response after publication still replays (step 2 precedes step 3).
    publish("docs", &version_id(&first));
    let late = stage(
        "docs",
        ASSET_A,
        "a.md",
        "hello world",
        &[(0, 5), (6, 11)],
        "k1",
        0,
    );
    assert_eq!(late["version_id"], first["version_id"]);

    let (state, reason) = call_err(
        "stage_version",
        "docs",
        stage_req(ASSET_A, "a.md", "hello there", &[(0, 5)], "k1", 1),
    );
    assert_eq!(
        (state.as_str(), reason.as_str()),
        ("23505", "ingestion_key_reused")
    );
}

#[pg_test]
fn new_asset_with_nonzero_revision_writes_nothing() {
    init("docs", json!({}));
    let (state, detail) = error_of(&call_sql(
        "stage_version",
        "docs",
        &stage_req(ASSET_B, "b.md", "x", &[], "kb", 1),
    ));
    assert_eq!(
        (state.as_str(), detail["reason"].as_str()),
        ("55000", Some("revision_conflict"))
    );
    assert_eq!(detail["current_revision"], 0);
    assert_eq!(int("SELECT count(*) FROM docs.assets"), 0);
    assert_eq!(int("SELECT count(*) FROM docs.versions"), 0);
}

#[pg_test]
fn stale_publish_conflicts_and_leaves_version_staged() {
    init("docs", json!({}));
    let a = stage("docs", ASSET_A, "a.md", "v1", &[(0, 2)], "k1", 0);
    publish("docs", &version_id(&a));
    let b1 = stage("docs", ASSET_A, "a.md", "v2 first", &[(0, 2)], "k2", 1);
    let b2 = stage("docs", ASSET_A, "a.md", "v2 second", &[(0, 2)], "k3", 1);
    publish("docs", &version_id(&b1));
    let (state, detail) = error_of(&call_sql(
        "publish_version",
        "docs",
        &json!({"version_id": version_id(&b2)}),
    ));
    assert_eq!(
        (state.as_str(), detail["reason"].as_str()),
        ("55000", Some("revision_conflict"))
    );
    assert_eq!(detail["current_revision"], 2);
    assert_eq!(detail["base_revision"], 1);
    assert_eq!(resolve("docs", &evidence_id(&b2, 0))["status"], "staged");

    // Publication is idempotent per version, even once superseded.
    let again = publish("docs", &version_id(&a));
    assert_eq!(
        (
            again["replayed"].as_bool(),
            again["revision"].as_i64(),
            again["current"].as_bool()
        ),
        (Some(true), Some(1), Some(false))
    );
    let (state, _) = call_err(
        "publish_version",
        "docs",
        json!({"version_id": "00000000-0000-4000-8000-000000000000"}),
    );
    assert_eq!(state, "22023");
}

#[pg_test]
fn current_path_collision_rolls_back_publish() {
    init("docs", json!({}));
    let a = stage("docs", ASSET_A, "same.md", "a", &[(0, 1)], "k1", 0);
    publish("docs", &version_id(&a));
    let b = stage("docs", ASSET_B, "same.md", "b", &[(0, 1)], "k2", 0);
    assert_eq!(
        sqlstate(&call_sql(
            "publish_version",
            "docs",
            &json!({"version_id": version_id(&b)})
        )),
        "23505"
    );
    assert_eq!(int("SELECT count(*) FROM docs.publications"), 1);
}

#[pg_test]
fn retire_then_republish() {
    init("docs", json!({}));
    with_timeout();
    let a = stage("docs", ASSET_A, "a.md", "retire me", &[(0, 6)], "k1", 0);
    publish("docs", &version_id(&a));
    assert_eq!(
        call_err(
            "retire",
            "docs",
            json!({"asset_id": ASSET_A, "expected_revision": 0})
        )
        .0,
        "55000"
    );
    let r = call(
        "retire",
        "docs",
        json!({"asset_id": ASSET_A, "expected_revision": 1}),
    );
    assert_eq!(
        (r["status"].as_str(), r["content_revision"].as_i64()),
        (Some("retired"), Some(2))
    );
    let old = resolve("docs", &evidence_id(&a, 0));
    assert_eq!(
        (old["status"].as_str(), old["text"].as_str()),
        (Some("retired"), Some("retire"))
    );
    let q = query(
        "docs",
        json!({"nodes": [{"id": "a", "op": "literal", "text": "retire"}], "output": "a"}),
    );
    assert_eq!(q["results"], json!([]));
    // A version staged against the new revision makes the asset current again.
    let c = stage("docs", ASSET_A, "a.md", "back again", &[(0, 4)], "k2", 2);
    assert_eq!(publish("docs", &version_id(&c))["revision"], 3);
    assert_eq!(
        int(&format!(
            "SELECT count(*) FROM docs.assets WHERE asset_id = {}::uuid AND retired_at IS NULL",
            lit(ASSET_A)
        )),
        1
    );
}

#[pg_test]
fn purge_tombstones_without_substitution() {
    init("docs", json!({}));
    let a = stage("docs", ASSET_A, "a.md", "secret text", &[(0, 6)], "k1", 0);
    publish("docs", &version_id(&a));
    let (state, reason) = call_err(
        "purge",
        "docs",
        json!({"version_id": version_id(&a), "reason": "test"}),
    );
    assert_eq!(
        (state.as_str(), reason.as_str()),
        ("55000", "version_current")
    );

    let b = stage("docs", ASSET_A, "a.md", "public text", &[(0, 6)], "k2", 1);
    publish("docs", &version_id(&b));
    call(
        "annotate",
        "docs",
        json!({"action": "link", "source_evidence_id": evidence_id(&b, 0),
                                    "target_evidence_id": evidence_id(&a, 0), "kind": "supersedes"}),
    );
    let p = call(
        "purge",
        "docs",
        json!({"version_id": version_id(&a), "reason": "privacy request"}),
    );
    assert_eq!(
        (
            p["status"].as_str(),
            p["replayed"].as_bool(),
            p["spans_purged"].as_i64()
        ),
        (Some("purged"), Some(false), Some(1))
    );

    let r = resolve("docs", &evidence_id(&a, 0));
    assert_eq!(r["status"], "purged");
    assert_eq!(r["reason"], "privacy request");
    assert!(r.get("text").is_none(), "{r}");
    assert_eq!(r["source_sha256"], sha("secret text"));
    assert_eq!(r["version_id"], a["version_id"]);
    assert_eq!(
        int("SELECT count(*) FROM docs.versions WHERE source IS NULL"),
        1
    );
    assert_eq!(
        int("SELECT count(*) FROM docs.spans WHERE text IS NULL AND tsv IS NULL"),
        1
    );
    assert_eq!(int("SELECT count(*) FROM docs.relations"), 1);
    assert_eq!(int("SELECT count(*) FROM docs.publications"), 2);

    let again = call(
        "purge",
        "docs",
        json!({"version_id": version_id(&a), "reason": "other"}),
    );
    assert_eq!(
        (again["replayed"].as_bool(), again["reason"].as_str()),
        (Some(true), Some("privacy request"))
    );
    // A purged version can never be republished.
    let (state, reason) = call_err(
        "publish_version",
        "docs",
        json!({"version_id": version_id(&a)}),
    );
    assert_eq!(
        (state.as_str(), reason.as_str()),
        ("00000", ""),
        "already published: idempotent replay"
    );
    let s = stage(
        "docs",
        ASSET_A,
        "a.md",
        "staged then purged",
        &[(0, 6)],
        "k3",
        2,
    );
    call(
        "purge",
        "docs",
        json!({"version_id": version_id(&s), "reason": "drop"}),
    );
    assert_eq!(
        call_err(
            "publish_version",
            "docs",
            json!({"version_id": version_id(&s)})
        ),
        ("55000".into(), "version_purged".into())
    );
}

#[pg_test]
fn corruption_is_detected_on_resolve() {
    init("docs", json!({}));
    let a = stage(
        "docs",
        ASSET_A,
        "a.md",
        "trusted bytes",
        &[(0, 7), (8, 13)],
        "k1",
        0,
    );
    publish("docs", &version_id(&a));
    // Same length so the row-local length CHECK still passes.
    exec(&format!(
        "UPDATE docs.spans SET text = 'TRUSTED' WHERE evidence_id = {}::uuid",
        lit(&evidence_id(&a, 0))
    ));
    let (state, detail) = error_of(&format!(
        "SELECT evidence.resolve('docs', {}::uuid)",
        lit(&evidence_id(&a, 0))
    ));
    assert_eq!(
        (state.as_str(), detail["reason"].as_str()),
        ("XX001", Some("span_mismatch"))
    );

    // The digest CHECK rejects a non-matching source; an owner can drop it.
    assert_eq!(
        sqlstate("UPDATE docs.versions SET source = 'trusted BYTES'"),
        "23514"
    );
    exec("ALTER TABLE docs.versions DROP CONSTRAINT versions_source_digest");
    exec("UPDATE docs.versions SET source = 'trusted BYTES'");
    let (state, detail) = error_of(&format!(
        "SELECT evidence.resolve('docs', {}::uuid)",
        lit(&evidence_id(&a, 1))
    ));
    assert_eq!(
        (state.as_str(), detail["reason"].as_str()),
        ("XX001", Some("digest_mismatch"))
    );
}

#[pg_test]
fn spans_are_stored_in_request_order_with_exact_slices() {
    init("docs", json!({}));
    // Several spans exercise the zipped ROWS FROM insert: each start/end/text
    // triple must land in one row.
    let src = "zero one two three \u{e9}four";
    let spans = [(19, 25), (0, 4), (5, 8), (9, 12), (13, 18)];
    let staged = stage("docs", ASSET_A, "s.md", src, &spans, "k1", 0);
    for (i, (s, e)) in spans.iter().enumerate() {
        assert_eq!(staged["spans"][i]["start_byte"], *s);
        let stored = text(&format!("SELECT text FROM docs.spans WHERE evidence_id = {}::uuid AND start_byte = {s} AND end_byte = {e}",
            lit(&evidence_id(&staged, i))));
        assert_eq!(stored.as_deref(), Some(&src[*s..*e]));
    }
    assert_eq!(int("SELECT count(*) FROM docs.spans"), 5);
}

#[pg_test]
fn non_truncatable_responses_obey_max_response_bytes() {
    init("docs", json!({"limits": {"max_response_bytes": 450}}));
    // 5 spans render to about 700 bytes, more than 450: refused before any write.
    let src = "aa bb cc dd ee";
    let spans = [(0, 2), (3, 5), (6, 8), (9, 11), (12, 14)];
    let (state, reason) = call_err(
        "stage_version",
        "docs",
        stage_req(ASSET_A, "a.md", src, &spans, "k1", 0),
    );
    assert_eq!(
        (state.as_str(), reason.as_str()),
        ("54000", "response_too_large")
    );
    assert_eq!(int("SELECT count(*) FROM docs.assets"), 0);
    let ok = stage("docs", ASSET_A, "a.md", src, &spans[..1], "k2", 0);
    assert!(
        response_bytes(
            "stage_version",
            "docs",
            &stage_req(ASSET_A, "a.md", src, &spans[..1], "k2", 0)
        ) <= 450
    );
    publish("docs", &version_id(&ok));
    // Resolve never cuts exact text; an over-budget citation is 54000.
    let long = "x".repeat(500);
    let big = stage("docs", ASSET_B, "b.md", &long, &[(0, 500)], "k3", 0);
    assert_eq!(
        sqlstate(&format!(
            "SELECT evidence.resolve('docs', {}::uuid)",
            lit(&evidence_id(&big, 0))
        )),
        "54000"
    );
    assert_eq!(resolve("docs", &evidence_id(&ok, 0))["text"], "aa");
}

#[pg_test]
fn request_validation_and_postgres_input_errors() {
    init("docs", json!({"limits": {"max_source_bytes": 16}}));
    let (state, reason) = call_err(
        "stage_version",
        "docs",
        stage_req(ASSET_A, "a.md", "seventeen bytes!!", &[], "k", 0),
    );
    assert_eq!(
        (state.as_str(), reason.as_str()),
        ("54000", "limit_exceeded")
    );
    let mut wrong = stage_req(ASSET_A, "a.md", "abc", &[], "k", 0);
    wrong["source_sha256"] = json!(sha("abd"));
    assert_eq!(
        call_err("stage_version", "docs", wrong),
        ("22023".into(), "source_sha256_mismatch".into())
    );
    assert_eq!(
        call_err(
            "stage_version",
            "docs",
            stage_req(ASSET_A, "a.md", "\u{e9}", &[(0, 1)], "k", 0)
        )
        .0,
        "22023"
    );
    assert_eq!(
        call_err("stage_version", "docs", json!({"asset_id": ASSET_A})).0,
        "22023"
    );
    // Rejected by PostgreSQL before the function runs.
    assert_eq!(
        sqlstate("SELECT evidence.stage_version('docs', '{bad json'::jsonb)"),
        "22P02"
    );
    assert_eq!(
        sqlstate("SELECT evidence.stage_version('docs', '{\"source\": \"\\u0000\"}'::jsonb)"),
        "22P05"
    );
    // Nesting deeper than the parser supports is a validation error, not a crash.
    let deep = format!("{{\"x\": {}{}}}", "[".repeat(300), "]".repeat(300));
    assert_eq!(
        sqlstate(&format!(
            "SELECT evidence.stage_version('docs', {}::jsonb)",
            lit(&deep)
        )),
        "22023"
    );
    assert_eq!(int("SELECT count(*) FROM docs.assets"), 0);
}
