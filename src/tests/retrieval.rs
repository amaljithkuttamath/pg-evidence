// Query modes, composition, budgets, readiness, embeddings and search_path.

fn seed_two_versions() -> (Value, Value) {
    init("docs", json!({}));
    let a = stage(
        "docs",
        ASSET_A,
        "guide/retry.md",
        "Retry semantics: old wording here",
        &[(0, 15), (17, 33)],
        "k1",
        0,
    );
    publish("docs", &version_id(&a));
    let b = stage(
        "docs",
        ASSET_A,
        "guide/retry.md",
        "Retry semantics: new wording here",
        &[(0, 15), (17, 33)],
        "k2",
        1,
    );
    publish("docs", &version_id(&b));
    (a, b)
}

#[pg_test]
fn query_requires_statement_timeout() {
    init("docs", json!({}));
    exec("SET LOCAL statement_timeout = 0");
    let (state, reason) = call_err(
        "query",
        "docs",
        json!({"nodes": [{"id": "a", "op": "literal", "text": "x"}], "output": "a"}),
    );
    assert_eq!(
        (state.as_str(), reason.as_str()),
        ("55000", "statement_timeout_unset")
    );
}

#[pg_test]
fn search_modes_return_current_evidence_only() {
    let (_a, b) = seed_two_versions();
    with_timeout();
    for (op, key, value) in [
        ("literal", "text", "wording"),
        ("regex", "pattern", "w[a-z]+ing"),
        ("lexical", "query", "wording"),
    ] {
        let q = query(
            "docs",
            json!({"nodes": [{"id": "a", "op": op, key: value}], "output": "a"}),
        );
        let results = q["results"].as_array().unwrap();
        assert_eq!(results.len(), 1, "{op}: {q}");
        assert_eq!(
            results[0]["evidence_id"].as_str(),
            Some(evidence_id(&b, 1).as_str()),
            "{op}"
        );
        assert_eq!(results[0]["status"], "current");
        assert_eq!(results[0]["mode"], op);
        assert_eq!(results[0]["excerpt"], "new wording here");
        assert_eq!(q["readiness"]["semantic"], "not_configured");
        assert_eq!(q["complete"], true);
    }
    let ci = query(
        "docs",
        json!({"nodes": [{"id": "a", "op": "regex", "pattern": "NEW", "case_insensitive": true}], "output": "a"}),
    );
    assert_eq!(ci["results"].as_array().unwrap().len(), 1);
    let filtered = query(
        "docs",
        json!({"nodes": [{"id": "a", "op": "literal", "text": "Retry", "filter": {"path_prefix": "other/"}}], "output": "a"}),
    );
    assert_eq!(filtered["results"], json!([]));
    assert_eq!(call_err("query", "docs", json!({"nodes": [{"id": "a", "op": "semantic", "model": "m", "vector": [1]}], "output": "a"})).1,
               "semantic_not_configured");
    assert_eq!(
        call_err(
            "query",
            "docs",
            json!({"nodes": [{"id": "a", "op": "regex", "pattern": "("}], "output": "a"})
        )
        .0,
        "2201B"
    );
}

#[pg_test]
fn response_budget_is_exact_and_whole_results_are_dropped() {
    init("docs", json!({}));
    with_timeout();
    let src = "needle one. needle two. needle three. needle four.";
    let spans: Vec<(usize, usize)> = src
        .match_indices("needle")
        .map(|(i, _)| (i, src[i..].find('.').unwrap() + i))
        .collect();
    let a = stage("docs", ASSET_A, "n.md", src, &spans, "k1", 0);
    publish("docs", &version_id(&a));
    let plan = |budget: Option<u64>| {
        let mut p =
            json!({"nodes": [{"id": "a", "op": "literal", "text": "needle"}], "output": "a"});
        if let Some(b) = budget {
            p["max_response_bytes"] = json!(b);
        }
        p
    };
    let full_len = response_bytes("query", "docs", &plan(None));
    let full = query("docs", plan(None));
    assert_eq!(full["results"].as_array().unwrap().len(), 4);
    // Budget equal to the full size keeps everything.
    assert_eq!(
        response_bytes("query", "docs", &plan(Some(full_len as u64))),
        full_len
    );
    let smaller = plan(Some(full_len as u64 - 1));
    let cut = query("docs", smaller.clone());
    assert!(response_bytes("query", "docs", &smaller) <= full_len - 1);
    let kept = cut["results"].as_array().unwrap().len();
    assert!(kept < 4);
    assert_eq!(cut["truncation"]["dropped_for_budget"], 4 - kept);
    assert_eq!(cut["truncation"]["truncated"], true);
    assert_eq!(cut["results"][0], full["results"][0]);
    let (state, reason) = call_err("query", "docs", plan(Some(40)));
    assert_eq!(
        (state.as_str(), reason.as_str()),
        ("54000", "response_envelope_too_large")
    );
    // Operator limits are reported, not silently applied.
    // Cut excerpts are reported and the response is not complete.
    let short = query(
        "docs",
        json!({"nodes": [{"id": "a", "op": "literal", "text": "needle"}], "output": "a", "excerpt_bytes": 3}),
    );
    assert_eq!(short["results"][0]["excerpt"], "nee");
    assert_eq!(short["results"][0]["excerpt_truncated"], true);
    assert_eq!(short["truncation"]["excerpts_truncated"], true);
    assert_eq!(short["complete"], false);
    let limited = query(
        "docs",
        json!({"nodes": [{"id": "a", "op": "literal", "text": "needle", "limit": 2}], "output": "a"}),
    );
    assert_eq!(
        limited["nodes"][0],
        json!({"id": "a", "op": "literal", "requested": 2, "returned": 2, "truncated": true, "underfilled": false})
    );
    assert_eq!(limited["complete"], false);
}

#[pg_test]
fn composed_plan_follows_one_hop_with_status() {
    let (a, b) = seed_two_versions();
    with_timeout();
    let other = stage(
        "docs",
        ASSET_B,
        "notes/cite.md",
        "cites retry doc",
        &[(0, 5)],
        "k3",
        0,
    );
    publish("docs", &version_id(&other));
    for (target, kind) in [
        (evidence_id(&b, 0), "cites"),
        (evidence_id(&a, 0), "cited_before"),
    ] {
        call(
            "annotate",
            "docs",
            json!({"action": "link", "source_evidence_id": evidence_id(&other, 0), "target_evidence_id": target, "kind": kind}),
        );
    }
    // Purge A so one endpoint is purged.
    call(
        "purge",
        "docs",
        json!({"version_id": version_id(&a), "reason": "test"}),
    );
    let q = query(
        "docs",
        json!({
            "nodes": [
                {"id": "hits", "op": "literal", "text": "cites"},
                {"id": "out", "op": "neighbors", "from": "hits", "direction": "out", "status": ["current", "purged"]},
                {"id": "all", "op": "union", "inputs": ["hits", "out"]}
            ],
            "output": "all"
        }),
    );
    let results = q["results"].as_array().unwrap();
    assert_eq!(results.len(), 3, "{q}");
    assert_eq!(results[0]["mode"], "literal");
    let purged = results
        .iter()
        .find(|r| r["status"] == "purged")
        .expect("purged endpoint");
    assert_eq!(purged["excerpt"], Value::Null);
    assert_eq!(
        purged["evidence_id"].as_str(),
        Some(evidence_id(&a, 0).as_str())
    );
    assert_eq!(q["edges"].as_array().unwrap().len(), 2);

    let only_current = query(
        "docs",
        json!({"nodes": [
        {"id": "hits", "op": "literal", "text": "cites"},
        {"id": "out", "op": "neighbors", "from": "hits", "status": ["current"], "kinds": ["cites"]}
    ], "output": "out"}),
    );
    assert_eq!(only_current["results"].as_array().unwrap().len(), 1);
    assert_eq!(only_current["edges"][0]["kind"], "cites");

    // Reverse direction from the cited evidence.
    let back = query(
        "docs",
        json!({"nodes": [
        {"id": "hits", "op": "literal", "text": "new wording"},
        {"id": "src", "op": "neighbors", "from": "hits", "direction": "in"}
    ], "output": "src"}),
    );
    assert_eq!(
        back["results"].as_array().unwrap().len(),
        0,
        "the link targets span 0, not span 1"
    );
}

#[pg_test]
fn plan_sql_is_not_injectable_and_search_path_is_pinned() {
    let (_a, _b) = seed_two_versions();
    with_timeout();
    // A hostile strpos/operator earlier on the caller's search_path must not be used.
    exec("CREATE FUNCTION public.strpos(text, text) RETURNS integer LANGUAGE sql AS 'SELECT 1'");
    exec("SET LOCAL search_path = public, pg_catalog");
    let q = query(
        "docs",
        json!({"nodes": [{"id": "a", "op": "literal", "text": "'); DROP TABLE docs.versions; --"}], "output": "a"}),
    );
    assert_eq!(q["results"], json!([]));
    assert_eq!(int("SELECT count(*) FROM docs.versions"), 2);
    let config = text("SELECT pg_catalog.array_to_string(proconfig, ',') FROM pg_catalog.pg_proc WHERE oid = 'evidence.query'::regproc");
    assert_eq!(config.as_deref(), Some("search_path=pg_catalog, pg_temp"));
}

#[pg_test]
fn query_sees_writes_earlier_in_the_transaction() {
    init("docs", json!({}));
    with_timeout();
    let a = stage(
        "docs",
        ASSET_A,
        "a.md",
        "fresh evidence",
        &[(0, 5)],
        "k1",
        0,
    );
    let before = query(
        "docs",
        json!({"nodes": [{"id": "a", "op": "literal", "text": "fresh"}], "output": "a"}),
    );
    assert_eq!(
        before["results"],
        json!([]),
        "staged evidence is not retrievable"
    );
    publish("docs", &version_id(&a));
    let after = query(
        "docs",
        json!({"nodes": [{"id": "a", "op": "literal", "text": "fresh"}], "output": "a"}),
    );
    assert_eq!(after["results"].as_array().unwrap().len(), 1);
}

#[pg_test]
fn embeddings_lifecycle_and_semantic_query() {
    exec("CREATE EXTENSION IF NOT EXISTS vector");
    init(
        "vec",
        json!({"embedding_model": "test-3d", "embedding_dimensions": 3}),
    );
    with_timeout();
    let a = stage(
        "vec",
        ASSET_A,
        "v.md",
        "north east south",
        &[(0, 5), (6, 10), (11, 16)],
        "k1",
        0,
    );
    let vid = version_id(&a);
    let (state, reason) = call_err("publish_version", "vec", json!({"version_id": vid}));
    assert_eq!(
        (state.as_str(), reason.as_str()),
        ("55000", "embeddings_missing")
    );

    let vectors = json!([
        {"evidence_id": evidence_id(&a, 0), "vector": [0, 1, 0]},
        {"evidence_id": evidence_id(&a, 1), "vector": [1, 0, 0]},
        {"evidence_id": evidence_id(&a, 2), "vector": [0, -1, 0.1]}
    ]);
    let req = json!({"version_id": vid, "model": "test-3d", "embeddings": vectors});
    // Partial attach, then the full set: the second call inserts only the
    // missing vector (multi-row ROWS FROM insert) and accepts the stored two.
    let mut partial = req.clone();
    partial["embeddings"] = json!([vectors[0].clone(), vectors[1].clone()]);
    call_void("attach_embeddings", "vec", partial);
    assert_eq!(int("SELECT count(*) FROM vec.embeddings"), 2);
    call_void("attach_embeddings", "vec", req.clone());
    assert_eq!(
        int(&format!(
            "SELECT count(*) FROM vec.embeddings e JOIN vec.spans s USING (evidence_id) \
        WHERE s.version_id = {}::uuid AND e.embedding = '[0,-1,0.1]'::vector",
            lit(&vid)
        )),
        1
    );
    call_void("attach_embeddings", "vec", req.clone()); // identical retry: no write
    assert_eq!(int("SELECT count(*) FROM vec.embeddings"), 3);
    let mut changed = req.clone();
    changed["embeddings"][0]["vector"] = json!([0, 1, 0.5]);
    assert_eq!(
        call_err("attach_embeddings", "vec", changed).1,
        "embedding_conflict"
    );
    let mut wrong_model = req.clone();
    wrong_model["model"] = json!("other");
    assert_eq!(call_err("attach_embeddings", "vec", wrong_model).0, "22023");
    let mut zero = req.clone();
    zero["embeddings"] = json!([{"evidence_id": evidence_id(&a, 0), "vector": [0, 0, 0]}]);
    assert_eq!(call_err("attach_embeddings", "vec", zero).0, "22023");

    publish("vec", &vid);
    call_void("attach_embeddings", "vec", req.clone()); // retry after publication still succeeds
    let q = query(
        "vec",
        json!({"nodes": [{"id": "s", "op": "semantic", "model": "test-3d", "vector": [0.1, 0.9, 0], "limit": 2}], "output": "s"}),
    );
    assert_eq!(q["readiness"]["semantic"], "ready");
    assert_eq!(q["approximate"], true);
    assert_eq!(q["complete"], false);
    let first = &q["results"][0];
    assert_eq!(
        first["evidence_id"].as_str(),
        Some(evidence_id(&a, 0).as_str())
    );
    assert_eq!(first["approximate"], true);
    assert!(first["distance"].as_f64().unwrap() < q["results"][1]["distance"].as_f64().unwrap());

    // New embeddings on a published version are refused; purge deletes embeddings.
    let b = stage("vec", ASSET_A, "v.md", "west", &[(0, 4)], "k2", 1);
    call_void(
        "attach_embeddings",
        "vec",
        json!({"version_id": version_id(&b), "model": "test-3d",
        "embeddings": [{"evidence_id": evidence_id(&b, 0), "vector": [-1, 0, 0]}]}),
    );
    publish("vec", &version_id(&b));
    let p = call("purge", "vec", json!({"version_id": vid, "reason": "test"}));
    assert_eq!(p["embeddings_deleted"], 3);
    assert_eq!(
        call_err("attach_embeddings", "vec", req).1,
        "version_purged"
    );
}

#[pg_test]
fn excerpt_materialization_cap_rejects_huge_output_payloads() {
    init(
        "big",
        json!({"limits": {"max_response_bytes": 16777216, "max_candidates_per_operator": 10000}}),
    );
    with_timeout();
    let a = stage("big", ASSET_A, "a.md", "needle text", &[(0, 6)], "k1", 0);
    publish("big", &version_id(&a));
    // 6 x 10000 x 16 MiB is far above 64 MiB: refused before any SQL runs.
    let (state, detail) = error_of(&call_sql(
        "query",
        "big",
        &json!({"nodes": [{"id": "a", "op": "literal", "text": "needle", "limit": 10000}],
                "output": "a", "excerpt_bytes": 16777216}),
    ));
    assert_eq!(
        (state.as_str(), detail["limit"].as_str()),
        ("54000", Some("max_excerpt_materialization_bytes"))
    );
    // Ordinary queries on the same large-budget collection still run.
    let ok = query(
        "big",
        json!({"nodes": [{"id": "a", "op": "literal", "text": "needle"}], "output": "a"}),
    );
    assert_eq!(ok["results"][0]["excerpt"], "needle");
    let wide = query(
        "big",
        json!({"nodes": [{"id": "a", "op": "literal", "text": "needle", "limit": 4096}],
               "output": "a", "excerpt_bytes": 2730}),
    );
    assert_eq!(wide["results"].as_array().map(Vec::len), Some(1));
}
