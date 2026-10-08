# Benchmark method and protocol freeze

Development protocol, 2026-10-07. Machine-readable protocol: `bench/protocol.json`.
Validator: `bench/protocol.py`. Tests: `bench/tests/test_protocol.py`. No benchmark,
pilot or agent evaluation has been run; nothing here is a measured result.

## Check commands

```sh
python3 -m unittest discover -s bench/tests -p test_protocol.py
python3 -m bench.protocol --check bench/protocol.json   # exit 0 valid, 1 invalid
# final stage only; LOCK is the separately committed final lock (none exists yet)
python3 -m bench.protocol --check PROTOCOL --final-lock LOCK
```

Python standard library only (observed with Python 3.14.3 on the host).

## Two explicit freeze stages (C12)

| Stage | `stage` value | Contents | Validator behavior |
|---|---|---|---|
| Task 1 freeze | `pre_pilot` | Comparators, contrasts, estimands, decision rule, sidedness, 0.75 token-ratio bound, -0.03 quality margin, database matrix, initial schedule, seeds, cache and timeout policy, claim scope | `post_pilot` must be `null` |
| Post-pilot freeze | `final` | Adds `post_pilot`: holdout size and justification, ANN quality target, named performance budgets, final schedule, agent runner/model/budget approval, external comparator status, any matrix narrowing | Every required `post_pilot` field must be present and typed, with no unknown keys; `frozen_before_holdout_measurement` must be `true`; the CLI also requires a matching final lock |

Every top-level field except `stage` and `post_pilot` is frozen. Its canonical JSON
(sorted keys, no whitespace, UTF-8, no NaN) is hashed with SHA-256 and compared with
`FROZEN_SHA256` in `bench/protocol.py`. An edit that is otherwise valid, for
example reordering seeds or editing a note, is rejected as `frozen protocol
modified`. Changing a frozen field therefore needs a visible, reviewed change to
that constant. Current digest:
`d96238b1aaaeda90c9e9d225bd5b682a19a119b1cd551e556c48e8045be698df`.

The post-pilot values are deliberately absent. The test-only values in
`FINAL_FIXTURE` exercise the final-stage validator and are not decisions.

## Final lock

`FROZEN_SHA256` covers only the Task 1 fields, so a well-formed edit to
`post_pilot` (for example a different holdout size, ANN target or budget) still
passes `validate_protocol`. That function is a structural check only; it is
not evidence that a final run used the frozen values.

The CLI therefore refuses a `final` protocol unless `--final-lock LOCK` is given.
`LOCK` is a separate JSON file, committed when the post-pilot values are frozen
and supplied by the runner, of exactly this form:

```json
{"schema": "pg-evidence/final-protocol-lock", "protocol_sha256": "<64 lowercase hex>"}
```

`protocol_sha256` is `bench.protocol.protocol_digest(protocol)`: SHA-256 of the
whole protocol's canonical JSON, including `stage` and `post_pilot`. Any later
change to the protocol fails against that lock (`does not match final lock`).
The CLI also rejects `--final-lock` on a `pre_pilot` protocol. No final lock or
real final digest exists; none should be created before the pilot.

An intentional refreeze, before any holdout measurement, needs a reviewed commit
that changes the protocol and the lock together, with the reason recorded. A
refreeze after holdout measurement has begun invalidates the predeclared final
analysis; report it as such rather than re-locking silently.

## Input strictness

The JSON loader rejects `NaN`/`Infinity`, floats that overflow (such as `1e400`)
and duplicate keys. The validator rejects booleans where numbers are expected,
integers beyond 2^53, non-string condition metrics, unknown nested `post_pilot`
keys, a `matrix_narrowing` that is not a list of known items, and narrowing that
removes every value of a dimension (alone or across items). All of these raise
`ValueError`, and the CLI exits 1 without a traceback. A test replaces each
field of both the committed and a final fixture protocol with several wrong types
and requires either acceptance or `ValueError`.

## What is frozen and why

- Required comparators (no unmeasured option): `sql_direct` (S-D), `sql_composed`
  (S-C), `extension_direct` (E-D), `extension_composed` (E-C), `files_ripgrep`,
  `pgvector_exact`, `pgvector_hnsw`.
- Listed external comparators: `paradedb_pg_search`, `pgvectorscale_diskann`
  (not optional; measured with a version, or unmeasured with a reason, at the
  final stage); `pg_textsearch` optional. Vendor numbers never substitute.
- Primary contrasts (C11): composition tokens E-C versus E-D; extension overhead
  E-C versus S-C. Replication: S-C versus S-D. Files/ripgrep is a workflow
  comparison that cannot attribute savings to the extension. Other contrasts are
  descriptive two-sided 95% intervals with no claims.
- Estimands: token ratio of mean total model tokens per attempted question
  (repetitions averaged within question, questions equally weighted), and the
  supported-answer-rate difference. Both use attempted questions as denominator;
  failed attempts count as unsupported, so early failures cannot become savings.
- Decision rule: one joint `all_of` rule. Token-ratio upper one-sided 95% bound
  `<= 0.75` AND supported-answer difference lower one-sided 95% bound `> -0.03`.
  The validator rejects changed thresholds, sidedness, confidence, bound direction,
  operator, `any_of`, a missing condition, booleans, strings and NaN.
- Database matrix: 100K/1M current spans, 1/8/32 clients, 1/5/20 retained versions,
  eligible fractions 1/0.1/0.01/0.001, correlated and uncorrelated filters. Final
  narrowing must name removed frozen values and a concrete resource limitation.
- Initial database schedule: 30 s warmup, 120 s measurement, five runs. These are
  pilot settings; the final schedule is a required post-pilot field.
- Timeouts, failures, late and skipped requests remain in denominators.

## Protocol choices made in Task 1

These were not specified numerically by the plan and are protocol choices, not
results. They are frozen in the digest; changing them requires a reviewed digest
update.

1. `claim_scope.min_independent_source_groups_for_cross_corpus_claim = 20` is a
   policy floor: below it, claims are limited to the evaluated corpora. Meeting
   it is necessary, not sufficient. It does not establish cross-corpus
   generalization or the precision of any estimate; those depend on how the
   groups were sampled, their heterogeneity and the cluster-level uncertainty
   actually observed. It is not a sample-size target.
2. `seeds = [101, 202, 303, 404, 505]`: arbitrary distinct values, one per
   initial run. Accepted as a project protocol choice.
3. `question_splits.pilot_questions_initial = 100`, copied from the plan's
   "approximately 100" pilot; explicitly not a justification for the holdout
   size. Accepted as a project protocol choice.

## Not decided here

Final holdout size, ANN quality target, performance budgets, final run schedule,
agent runner/model and evaluation budget are post-pilot or owner decisions. The
validator refuses a `final` protocol without them and accepts no `pre_pilot`
protocol that contains them.

The public snapshot updates source-document paths only; benchmark rules are unchanged.

## Development SQL comparison runner

`python3 -m bench.compare_sql` performs a small, paired comparison before the
full performance harness is available. Each SQL file must return one JSON value.
Use a quiescent fixture database and the same roles, indexes and output semantics.
Connection settings come from standard `PGHOST`, `PGPORT`, `PGUSER`, `PGDATABASE`
and libpq authentication settings.

```sh
python3 -m bench.compare_sql --baseline baseline.sql --candidate candidate.sql \
  --repetitions 5 --timeout-ms 10000 --output bench/results/smoke-001
```

The runner shuffles arm order with seed 101, enforces read-only transactions and
statement timeouts, checks canonical JSON equality for every pair, and writes each
observation immediately. Failed queries and result mismatches stop the comparison
with a nonzero exit and remain in the raw log. Output directories must be new.

The reported client times include a new `psql` process and database connection for
each sample. There is no warmup, controlled cache reset, concurrency load, memory
measurement or statistical claim in this smoke runner. It does not satisfy the
frozen pilot or final protocol and cannot establish an extension speedup, ANN
quality or token savings. Query hashes and raw observations make the small check
inspectable; a release measurement still needs the full environment and dataset
manifest required above.

The Docker product check also runs `bench/sql/smoke_fixture.sql` (1,000 synthetic
ASCII sources) and compares `smoke_sql.sql` with `smoke_extension.sql`. It checks
identical literal result projections over the same tables. The extension arm also
pays for its API validation and envelope rendering before projecting the results;
the SQL arm returns only the result projection. This is a sanity check, not the
fully matched response-encoding baseline required for release. No semantic or
agent-quality conclusion follows from this synthetic fixture.

The broader `baseline/wrapper.sql` control has known serialization and
envelope-budget differences listed in [its README](../baseline/README.md).
Resolve those differences before claiming matched-budget latency improvements.
Vector compression candidates and the measurements they require are tracked in
[the compression experiment](vector-compression.md); TurboQuant is not enabled.
