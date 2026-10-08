// G3 function attributes, corpus catalog independence (design "Schemas"),
// collection naming and configuration errors.

#[pg_test]
fn api_functions_have_declared_attributes() {
    let rows = text(
        "SELECT pg_catalog.string_agg(p.proname::text || ':' || p.provolatile::text || p.proparallel::text || \
         p.proisstrict::text || p.prosecdef::text || ':' || pg_catalog.array_to_string(p.proconfig, ',') || ':' || \
         pg_catalog.pg_get_function_identity_arguments(p.oid) || ':' || pg_catalog.format_type(p.prorettype, NULL), \
         E'\\n' ORDER BY p.proname) \
         FROM pg_catalog.pg_proc p WHERE p.pronamespace = 'evidence'::regnamespace",
    )
    .expect("functions exist");
    let expected = [
        "annotate:vutruefalse:search_path=pg_catalog, pg_temp:corpus text, request jsonb:json",
        "attach_embeddings:vutruefalse:search_path=pg_catalog, pg_temp:corpus text, request jsonb:void",
        "init_collection:vutruefalse:search_path=pg_catalog, pg_temp:corpus text, config jsonb:void",
        "publish_version:vutruefalse:search_path=pg_catalog, pg_temp:corpus text, request jsonb:json",
        "purge:vutruefalse:search_path=pg_catalog, pg_temp:corpus text, request jsonb:json",
        "query:sutruefalse:search_path=pg_catalog, pg_temp:corpus text, request jsonb:json",
        "resolve:sutruefalse:search_path=pg_catalog, pg_temp:corpus text, evidence_id uuid:json",
        "retire:vutruefalse:search_path=pg_catalog, pg_temp:corpus text, request jsonb:json",
        "stage_version:vutruefalse:search_path=pg_catalog, pg_temp:corpus text, request jsonb:json",
    ]
    .join("\n");
    assert_eq!(rows, expected);
}

#[pg_test]
fn corpus_objects_are_not_extension_members_and_do_not_depend_on_it() {
    init("docs", json!({}));
    // Dependent objects that belong to the corpus schema.
    let corpus_deps = "pg_catalog.pg_depend d WHERE ( \
        (d.classid = 'pg_catalog.pg_class'::regclass AND d.objid IN \
           (SELECT c.oid FROM pg_catalog.pg_class c WHERE c.relnamespace = 'docs'::regnamespace)) \
        OR (d.classid = 'pg_catalog.pg_constraint'::regclass AND d.objid IN \
           (SELECT k.oid FROM pg_catalog.pg_constraint k WHERE k.connamespace = 'docs'::regnamespace)) \
        OR (d.classid = 'pg_catalog.pg_attrdef'::regclass AND d.objid IN \
           (SELECT a.oid FROM pg_catalog.pg_attrdef a JOIN pg_catalog.pg_class c ON c.oid = a.adrelid \
            WHERE c.relnamespace = 'docs'::regnamespace)) \
        OR (d.classid = 'pg_catalog.pg_type'::regclass AND d.objid IN \
           (SELECT t.oid FROM pg_catalog.pg_type t WHERE t.typnamespace = 'docs'::regnamespace)) \
        OR (d.classid = 'pg_catalog.pg_namespace'::regclass AND d.objid = 'docs'::regnamespace))";
    assert!(
        int(&format!("SELECT count(*) FROM {corpus_deps}")) > 0,
        "catalog query matched nothing"
    );
    let on_extension = int(&format!(
        "SELECT count(*) FROM {corpus_deps} AND ( \
           (d.refclassid = 'pg_catalog.pg_extension'::regclass) \
           OR (d.refclassid = 'pg_catalog.pg_namespace'::regclass AND d.refobjid = 'evidence'::regnamespace) \
           OR (d.refclassid = 'pg_catalog.pg_proc'::regclass AND d.refobjid IN \
               (SELECT p.oid FROM pg_catalog.pg_proc p WHERE p.pronamespace = 'evidence'::regnamespace)))"
    ));
    let membership = int(
        "SELECT count(*) FROM pg_catalog.pg_depend d \
         JOIN pg_catalog.pg_class c ON d.classid = 'pg_catalog.pg_class'::regclass AND d.objid = c.oid \
         WHERE d.deptype = 'e' AND c.relnamespace = 'docs'::regnamespace",
    );
    assert_eq!((on_extension, membership), (0, 0));
    assert_eq!(text("SELECT pg_catalog.pg_get_userbyid(nspowner)::text FROM pg_catalog.pg_namespace WHERE nspname = 'docs'"),
               text("SELECT current_user::text"));
    assert_eq!(int("SELECT count(*) FROM pg_catalog.pg_trigger t JOIN pg_catalog.pg_class c ON c.oid = t.tgrelid \
                    WHERE c.relnamespace = 'docs'::regnamespace AND NOT t.tgisinternal"), 0);
    assert_eq!(
        int("SELECT schema_version::bigint FROM docs.collection_config"),
        1
    );
}

#[pg_test]
fn embeddings_depend_only_on_pgvector() {
    exec("CREATE EXTENSION IF NOT EXISTS vector");
    init(
        "vec_docs",
        json!({"embedding_model": "m", "embedding_dimensions": 3}),
    );
    let ext_deps = text(
        "SELECT pg_catalog.string_agg(DISTINCT e.extname::text, ',') FROM pg_catalog.pg_depend d \
         JOIN pg_catalog.pg_type t ON d.refclassid = 'pg_catalog.pg_type'::regclass AND d.refobjid = t.oid \
         JOIN pg_catalog.pg_depend m ON m.classid = 'pg_catalog.pg_type'::regclass AND m.objid = t.oid AND m.deptype = 'e' \
         JOIN pg_catalog.pg_extension e ON e.oid = m.refobjid \
         WHERE d.classid = 'pg_catalog.pg_class'::regclass AND d.objid = 'vec_docs.embeddings'::regclass",
    );
    assert_eq!(ext_deps.as_deref(), Some("vector"));
}

#[pg_test]
fn collection_names_and_config_are_validated() {
    init("docs", json!({}));
    assert_eq!(
        sqlstate("SELECT evidence.init_collection('docs', '{}'::jsonb)"),
        "42P06"
    );
    for bad in [
        "Docs",
        "pg_docs",
        "public",
        "evidence",
        "information_schema",
        "a-b",
        "x\"y",
    ] {
        assert_eq!(
            sqlstate(&format!(
                "SELECT evidence.init_collection({}, '{{}}'::jsonb)",
                lit(bad)
            )),
            "22023",
            "{bad}"
        );
    }
    assert_eq!(
        sqlstate(
            "SELECT evidence.init_collection('d2', '{\"text_search_config\": \"no_such\"}'::jsonb)"
        ),
        "22023"
    );
    assert_eq!(
        sqlstate("SELECT evidence.init_collection('d3', '{\"bogus\": 1}'::jsonb)"),
        "22023"
    );
    // pgvector absent (not created in this transaction) with embeddings configured.
    if int("SELECT count(*) FROM pg_catalog.pg_extension WHERE extname = 'vector'") == 0 {
        let (state, detail) = error_of("SELECT evidence.init_collection('d4', '{\"embedding_model\": \"m\", \"embedding_dimensions\": 3}'::jsonb)");
        assert_eq!(
            (state.as_str(), detail["reason"].as_str()),
            ("55000", Some("pgvector_missing"))
        );
    }
    // A failed init leaves no schema behind.
    assert_eq!(
        int("SELECT count(*) FROM pg_catalog.pg_namespace WHERE nspname IN ('d2', 'd3', 'd4')"),
        0
    );
    // Unknown corpus: PostgreSQL's own undefined-table error.
    assert_eq!(
        sqlstate("SELECT evidence.query('nope', '{}'::jsonb)"),
        "42P01"
    );
    // STRICT: NULL arguments return NULL without running.
    assert_eq!(
        text("SELECT evidence.stage_version(NULL, '{}'::jsonb)::text"),
        None
    );
    // Non-English configuration is stored schema-qualified.
    init("en_docs", json!({"text_search_config": "english"}));
    assert_eq!(
        text("SELECT text_search_config FROM en_docs.collection_config").as_deref(),
        Some("pg_catalog.english")
    );
}

#[pg_test]
fn unsupported_schema_version_is_refused() {
    init("docs", json!({}));
    exec("UPDATE docs.collection_config SET schema_version = 2");
    let (state, detail) = error_of(&call_sql("stage_version", "docs", &json!({})));
    assert_eq!(
        (state.as_str(), detail["reason"].as_str()),
        ("55000", Some("unsupported_schema_version"))
    );
    assert!(detail["hint"].as_str().is_some());
    exec("SET LOCAL statement_timeout = '10s'");
    assert_eq!(
        sqlstate("SELECT evidence.resolve('docs', gen_random_uuid())"),
        "55000"
    );
}
