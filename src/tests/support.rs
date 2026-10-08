// Shared helpers for the backend tests. Included into `mod tests` in lib.rs,
// so this file has no `use` items of its own. Each #[pg_test] runs in one
// transaction that the pgrx runner rolls back.

const ASSET_A: &str = "0b5f3c1e-8d2a-4f6b-9c3d-2e1f0a9b8c7d";
const ASSET_B: &str = "1c6a4d2f-9e3b-4a7c-8d4e-3f2a1b0c9d8e";

fn lit(s: &str) -> String {
    format!("'{}'", s.replace('\'', "''"))
}

fn exec(sql: &str) {
    Spi::run(sql).unwrap_or_else(|e| panic!("{sql}: {e}"));
}

fn text(sql: &str) -> Option<String> {
    Spi::get_one::<String>(sql).unwrap_or_else(|e| panic!("{sql}: {e}"))
}

fn int(sql: &str) -> i64 {
    Spi::get_one::<i64>(sql)
        .unwrap_or_else(|e| panic!("{sql}: {e}"))
        .expect("non-null")
}

/// Creates pg_temp.pgev_error(sql) returning '00000' or "<SQLSTATE> <detail>".
/// Installed once per transaction, before any SET ROLE, by its first caller.
fn install_error_probe() {
    let installed = int(
        "SELECT count(*) FROM pg_catalog.pg_proc WHERE proname = 'pgev_error' \
         AND pronamespace = pg_catalog.pg_my_temp_schema()",
    );
    if installed > 0 {
        return;
    }
    exec(
        "CREATE FUNCTION pg_temp.pgev_error(q text) RETURNS text LANGUAGE plpgsql AS $f$ \
         DECLARE d text; \
         BEGIN EXECUTE q; RETURN '00000'; \
         EXCEPTION WHEN OTHERS THEN GET STACKED DIAGNOSTICS d = PG_EXCEPTION_DETAIL; \
         RETURN SQLSTATE || ' ' || coalesce(d, ''); END $f$",
    );
}

/// (SQLSTATE, detail) from running `sql` in a subtransaction.
fn error_of(sql: &str) -> (String, Value) {
    install_error_probe();
    let out = text(&format!("SELECT pg_temp.pgev_error({})", lit(sql))).expect("probe result");
    let (state, detail) = out.split_once(' ').unwrap_or((out.as_str(), ""));
    let detail = serde_json::from_str(detail).unwrap_or(Value::Null);
    (state.to_string(), detail)
}

fn sqlstate(sql: &str) -> String {
    error_of(sql).0
}

fn call_sql(func: &str, corpus: &str, request: &Value) -> String {
    format!(
        "SELECT evidence.{func}({}, {}::jsonb)",
        lit(corpus),
        lit(&request.to_string())
    )
}

/// Calls a json-returning API function and parses its exact output.
fn call(func: &str, corpus: &str, request: Value) -> Value {
    let out = text(&format!(
        "SELECT ({})::text",
        call_sql(func, corpus, &request)
    ))
    .expect("non-null response");
    serde_json::from_str(&out).unwrap_or_else(|e| panic!("invalid JSON from {func}: {e}: {out}"))
}

/// Calls a void API function.
fn call_void(func: &str, corpus: &str, request: Value) {
    exec(&call_sql(func, corpus, &request));
}

/// (SQLSTATE, detail.reason) of an API call that must fail.
fn call_err(func: &str, corpus: &str, request: Value) -> (String, String) {
    let (state, detail) = error_of(&call_sql(func, corpus, &request));
    let reason = detail
        .get("reason")
        .and_then(Value::as_str)
        .unwrap_or("")
        .to_string();
    (state, reason)
}

fn sha(s: &str) -> String {
    crate::model::hex(&crate::model::sha256(s.as_bytes()))
}

fn init(corpus: &str, config: Value) {
    exec(&format!(
        "SELECT evidence.init_collection({}, {}::jsonb)",
        lit(corpus),
        lit(&config.to_string())
    ));
}

fn stage_req(
    asset: &str,
    path: &str,
    source: &str,
    spans: &[(usize, usize)],
    key: &str,
    expected: i64,
) -> Value {
    let spans: Vec<Value> = spans
        .iter()
        .map(|(s, e)| json!({"start_byte": s, "end_byte": e}))
        .collect();
    json!({
        "asset_id": asset, "path": path, "source": source, "source_sha256": sha(source),
        "spans": spans, "ingestion_key": key, "expected_revision": expected
    })
}

fn stage(
    corpus: &str,
    asset: &str,
    path: &str,
    source: &str,
    spans: &[(usize, usize)],
    key: &str,
    expected: i64,
) -> Value {
    call(
        "stage_version",
        corpus,
        stage_req(asset, path, source, spans, key, expected),
    )
}

fn publish(corpus: &str, version_id: &str) -> Value {
    call("publish_version", corpus, json!({"version_id": version_id}))
}

fn resolve(corpus: &str, evidence_id: &str) -> Value {
    let out = text(&format!(
        "SELECT evidence.resolve({}, {}::uuid)::text",
        lit(corpus),
        lit(evidence_id)
    ))
    .expect("non-null resolve");
    serde_json::from_str(&out).expect("valid resolve JSON")
}

fn evidence_id(staged: &Value, i: usize) -> String {
    staged["spans"][i]["evidence_id"]
        .as_str()
        .expect("evidence_id")
        .to_string()
}

fn version_id(staged: &Value) -> String {
    staged["version_id"]
        .as_str()
        .expect("version_id")
        .to_string()
}

fn with_timeout() {
    exec("SET LOCAL statement_timeout = '60s'");
}

fn query(corpus: &str, plan: Value) -> Value {
    call("query", corpus, plan)
}

/// Exact byte length of the json text a call returns.
fn response_bytes(func: &str, corpus: &str, request: &Value) -> i64 {
    int(&format!(
        "SELECT pg_catalog.octet_length(({})::text)::bigint",
        call_sql(func, corpus, request)
    ))
}
