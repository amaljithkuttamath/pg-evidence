// Tags scope assets with their own revision; relations bind evidence IDs.

#[pg_test]
fn tags_use_annotation_revision_independently_of_content() {
    init("docs", json!({}));
    with_timeout();
    let a = stage("docs", ASSET_A, "a.md", "tagged text", &[(0, 6)], "k1", 0);
    publish("docs", &version_id(&a));

    let t = call(
        "annotate",
        "docs",
        json!({"action": "tag", "asset_id": ASSET_A, "tags": ["spec", "v1"], "expected_annotation_revision": 0}),
    );
    assert_eq!(
        t,
        json!({"status": "annotated", "action": "tag", "asset_id": ASSET_A, "annotation_revision": 1, "added": ["spec", "v1"]})
    );
    let (state, reason) = call_err(
        "annotate",
        "docs",
        json!({"action": "tag", "asset_id": ASSET_A, "tags": ["x"], "expected_annotation_revision": 0}),
    );
    assert_eq!(
        (state.as_str(), reason.as_str()),
        ("55000", "revision_conflict")
    );
    // Tagging does not change content_revision, so a version staged at 1 still publishes.
    let b = stage("docs", ASSET_A, "a.md", "tagged again", &[(0, 6)], "k2", 1);
    assert_eq!(publish("docs", &version_id(&b))["revision"], 2);

    // Tags carry across versions: the filter finds the new current version.
    let q = query(
        "docs",
        json!({"nodes": [{"id": "a", "op": "literal", "text": "tagged", "filter": {"tags_all": ["spec", "v1"]}}], "output": "a"}),
    );
    assert_eq!(
        q["results"][0]["evidence_id"].as_str(),
        Some(evidence_id(&b, 0).as_str())
    );
    let none = query(
        "docs",
        json!({"nodes": [{"id": "a", "op": "literal", "text": "tagged", "filter": {"tags_any": ["other"]}}], "output": "a"}),
    );
    assert_eq!(none["results"], json!([]));

    let u = call(
        "annotate",
        "docs",
        json!({"action": "untag", "asset_id": ASSET_A, "tags": ["v1", "absent"], "expected_annotation_revision": 1}),
    );
    assert_eq!(
        (u["removed"].clone(), u["annotation_revision"].as_i64()),
        (json!(["v1"]), Some(2))
    );
    assert_eq!(call_err("annotate", "docs", json!({"action": "tag", "asset_id": ASSET_B, "tags": ["x"], "expected_annotation_revision": 0})).1, "unknown_asset");
}

#[pg_test]
fn links_are_idempotent_and_validated() {
    init("docs", json!({}));
    let a = stage(
        "docs",
        ASSET_A,
        "a.md",
        "claim and support",
        &[(0, 5), (10, 17)],
        "k1",
        0,
    );
    let (src, dst) = (evidence_id(&a, 0), evidence_id(&a, 1));
    let link = json!({"action": "link", "source_evidence_id": src, "target_evidence_id": dst, "kind": "supported_by"});
    let first = call("annotate", "docs", link.clone());
    assert_eq!(first["status"], "linked");
    let second = call("annotate", "docs", link.clone());
    assert_eq!(second["status"], "existing");
    assert_eq!(second["asserted_at"], first["asserted_at"]);
    // Reciprocal links (cycles) are allowed; self-links are not.
    let back = call(
        "annotate",
        "docs",
        json!({"action": "link", "source_evidence_id": dst, "target_evidence_id": src, "kind": "supported_by"}),
    );
    assert_eq!(back["status"], "linked");
    assert_eq!(call_err("annotate", "docs", json!({"action": "link", "source_evidence_id": src, "target_evidence_id": src, "kind": "x"})).0, "22023");
    assert_eq!(
        call_err(
            "annotate",
            "docs",
            json!({"action": "link", "source_evidence_id": src,
        "target_evidence_id": "00000000-0000-4000-8000-000000000000", "kind": "x"})
        )
        .1,
        "unknown_evidence"
    );

    let mut unlink = link.clone();
    unlink["action"] = json!("unlink");
    assert_eq!(
        call("annotate", "docs", unlink.clone())["status"],
        "unlinked"
    );
    assert_eq!(call("annotate", "docs", unlink)["status"], "absent");
    assert_eq!(int("SELECT count(*) FROM docs.relations"), 1);
}

#[pg_test]
fn link_after_removal_inserts_a_fresh_relation() {
    // The insert-then-select retry path handles a relation removed between the
    // two statements. A concurrent unlink cannot be interleaved inside one
    // backend test; this checks the sequential form: a removed relation is
    // re-linked as a new row, never reported as "existing" or as an error.
    init("docs", json!({}));
    let a = stage(
        "docs",
        ASSET_A,
        "a.md",
        "claim and support",
        &[(0, 5), (10, 17)],
        "k1",
        0,
    );
    let link = json!({"action": "link", "source_evidence_id": evidence_id(&a, 0),
                      "target_evidence_id": evidence_id(&a, 1), "kind": "cites"});
    assert_eq!(call("annotate", "docs", link.clone())["status"], "linked");
    exec("DELETE FROM docs.relations");
    assert_eq!(call("annotate", "docs", link.clone())["status"], "linked");
    assert_eq!(call("annotate", "docs", link)["status"], "existing");
    assert_eq!(int("SELECT count(*) FROM docs.relations"), 1);
}
