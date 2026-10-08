"""Benchmark protocol validator (Task 1; decisions C11 and C12).

Standard library only. ``validate_protocol`` raises ``ValueError`` on any
structural violation. ``python -m bench.protocol --check PATH [--final-lock LOCK]``
exits 0 for a valid protocol and 1 otherwise.

Two freeze stages are explicit. ``pre_pilot`` carries the Task 1 freeze
(comparators, contrasts, estimands, decision rule, sidedness, margins) and no
post-pilot values. ``final`` additionally requires the values frozen after the
pilot and before any holdout measurement: holdout size, ANN quality target,
performance budgets, final schedule, agent configuration and external
comparator status. Every Task 1 frozen top-level field is covered by
FROZEN_SHA256, so an edit that is otherwise semantically valid is still rejected.

Structural validity of a ``final`` protocol is not a locked final run. The CLI
accepts a final protocol only with ``--final-lock``: a separately committed
lock file whose ``protocol_sha256`` equals ``protocol_digest`` of the whole
protocol, including ``post_pilot``. Any later edit, even a well-formed one,
fails against that lock; an intentional refreeze needs a new reviewed lock.
"""

import argparse
import datetime
import hashlib
import json
import math
import sys

# SHA-256 of the canonical JSON of the frozen fields (see frozen_digest).
# Changing a frozen field requires a reviewed change to this constant.
FROZEN_SHA256 = "d96238b1aaaeda90c9e9d225bd5b682a19a119b1cd551e556c48e8045be698df"

REQUIRED_COMPARATORS = (
    "sql_direct", "sql_composed", "extension_direct", "extension_composed",
    "files_ripgrep", "pgvector_exact", "pgvector_hnsw",
)
LISTED_EXTERNAL = ("paradedb_pg_search", "pgvectorscale_diskann")
STAGES = ("pre_pilot", "final")
MUTABLE_KEYS = ("stage", "post_pilot")
FROZEN_KEYS = (
    "schema", "schema_version", "sources", "comparators", "comparator_definitions",
    "external_comparators", "external_comparator_rule", "primary_contrasts",
    "replication_contrasts", "workflow_comparisons", "secondary_contrasts",
    "estimands", "decision_rule", "claim_scope", "question_splits",
    "database_matrix", "initial_database_schedule", "seeds", "cache_policy",
    "timeout_accounting",
)
EXPECTED_RULE = {
    "token_ratio": {"bound": "upper", "sided": "one", "confidence": 0.95,
                    "operator": "<=", "threshold": 0.75},
    "supported_answer_rate_difference": {"bound": "lower", "sided": "one",
                                         "confidence": 0.95, "operator": ">",
                                         "threshold": -0.03},
}
EXPECTED_MATRIX = {
    "scales_spans": [100000, 1000000],
    "clients": [1, 8, 32],
    "retained_versions": [1, 5, 20],
    "filter_eligible_fraction": [1.0, 0.1, 0.01, 0.001],
}
POST_PILOT_REQUIRED = (
    "frozen_on", "pilot_evidence", "frozen_before_holdout_measurement",
    "final_holdout", "ann_quality_target", "performance_budgets",
    "database_schedule", "agent_configuration", "external_comparator_status",
)
POST_PILOT_KEYS = POST_PILOT_REQUIRED + ("matrix_narrowing",)
BUDGET_OPERATORS = ("<=", "<", ">=", ">")
BUDGET_KEYS = ("metric", "operator", "value", "unit", "workload", "machine")
SCHEDULE_KEYS = ("warmup_seconds", "measure_seconds", "runs")
NARROWING_KEYS = ("dimension", "removed", "resource_limitation")
LOCK_SCHEMA = "pg-evidence/final-protocol-lock"
# Larger integers lose exactness as JSON numbers in many readers and overflow
# float conversion; reject them rather than raising OverflowError.
MAX_EXACT_INT = 2 ** 53


# ---------------------------------------------------------------- type helpers

def _is_int(v):
    return isinstance(v, int) and not isinstance(v, bool) and abs(v) <= MAX_EXACT_INT


def _is_number(v):
    if isinstance(v, float):
        return math.isfinite(v)
    return _is_int(v)


def _pos_int(v, where):
    if not _is_int(v) or v <= 0:
        raise ValueError(f"{where} must be a positive integer, got {v!r:.40}")


def _finite(v, where):
    if not _is_number(v):
        raise ValueError(f"{where} must be a finite number, got {v!r:.40}")


def _text(v, where):
    if not isinstance(v, str) or not v.strip():
        raise ValueError(f"{where} must be a non-empty string")


def _obj(v, where, allowed=None):
    if not isinstance(v, dict):
        raise ValueError(f"{where} must be an object")
    if allowed is not None:
        unknown = set(v) - set(allowed)
        if unknown:
            raise ValueError(f"unknown keys in {where}: {sorted(unknown)}")
    return v


def _same_number(a, b):
    return _is_number(a) and a == b


def _same_list(actual, expected, where, kind):
    if not isinstance(actual, list) or not all(kind(x) for x in actual):
        raise ValueError(f"{where} has invalid element types")
    if actual != expected:
        raise ValueError(f"{where} must be {expected}, got {actual}")


# ------------------------------------------------------------------- digest

def _canonical_sha256(value):
    canonical = json.dumps(value, sort_keys=True, separators=(",", ":"),
                           ensure_ascii=False, allow_nan=False)
    return hashlib.sha256(canonical.encode("utf-8")).hexdigest()


def frozen_digest(protocol):
    """Digest of the Task 1 frozen fields only (compared with FROZEN_SHA256)."""
    return _canonical_sha256({k: protocol.get(k) for k in FROZEN_KEYS})


def protocol_digest(protocol):
    """Digest of the whole protocol, including stage and post_pilot (final lock)."""
    return _canonical_sha256(protocol)


# ------------------------------------------------------------- frozen fields

def _check_comparators(p):
    comps = p.get("comparators", [])
    if not isinstance(comps, list) or not all(isinstance(c, str) for c in comps):
        raise ValueError("comparators must be a list of names")
    if len(set(comps)) != len(comps):
        raise ValueError("duplicate comparator")
    missing = [c for c in REQUIRED_COMPARATORS if c not in comps]
    if missing:
        raise ValueError(f"missing required comparator: {', '.join(missing)}")

    external = _obj(p.get("external_comparators", {}), "external_comparators")
    for name in LISTED_EXTERNAL:
        if name not in external:
            raise ValueError(f"external comparison must be listed, measured or not: {name}")
    for name, spec in external.items():
        if name in REQUIRED_COMPARATORS:
            raise ValueError(f"required comparator {name} cannot be external or unmeasured")
        spec = _obj(spec, f"external_comparators.{name}")
        if not isinstance(spec.get("optional"), bool):
            raise ValueError(f"external_comparators.{name}.optional must be a boolean")
        if name in LISTED_EXTERNAL and spec["optional"]:
            raise ValueError(f"{name} is a listed external comparison, not optional")


def _check_contrasts(p):
    contrasts = _obj(p.get("primary_contrasts"), "primary_contrasts")
    if contrasts.get("composition_tokens") != ["extension_composed", "extension_direct"]:
        raise ValueError("composition claim must name E-C versus E-D")
    if contrasts.get("extension_overhead") != ["extension_composed", "sql_composed"]:
        raise ValueError("extension claim must name E-C versus S-C")
    if set(contrasts) != {"composition_tokens", "extension_overhead"}:
        raise ValueError("primary_contrasts has unexpected entries")

    rep = _obj(p.get("replication_contrasts"), "replication_contrasts")
    if rep.get("composition_replication") != ["sql_composed", "sql_direct"]:
        raise ValueError("composition replication must name S-C versus S-D")

    wf = _obj(p.get("workflow_comparisons"), "workflow_comparisons")
    files = _obj(wf.get("files_ripgrep"), "workflow_comparisons.files_ripgrep")
    if files.get("attributes_savings_to_extension") is not False:
        raise ValueError("files_ripgrep is a workflow comparison and cannot attribute savings")

    sec = _obj(p.get("secondary_contrasts"), "secondary_contrasts")
    if sec.get("interval") != "two_sided_95" or sec.get("claims") is not False:
        raise ValueError("secondary contrasts are descriptive two-sided 95% intervals without claims")


def _check_estimands_and_rule(p):
    est = _obj(p.get("estimands"), "estimands")
    for metric in EXPECTED_RULE:
        e = _obj(est.get(metric), f"estimands.{metric}")
        if e.get("contrast") != "composition_tokens":
            raise ValueError(f"estimands.{metric} must use the composition_tokens contrast")
        if e.get("denominator") != "attempted_questions":
            raise ValueError(f"estimands.{metric} denominator must be attempted questions")
        _text(e.get("definition"), f"estimands.{metric}.definition")
    if est["supported_answer_rate_difference"].get("failed_attempts") != "counted_as_unsupported":
        raise ValueError("failed attempts must count as unsupported answers")

    rule = _obj(_obj(p.get("decision_rule"), "decision_rule").get("composition_claim"),
                "decision_rule.composition_claim")
    if rule.get("contrast") != "composition_tokens":
        raise ValueError("decision rule must test the composition_tokens contrast")
    conditions = rule.get("all_of")
    if not isinstance(conditions, list) or set(rule) - {"contrast", "all_of", "multiplicity", "release_gate"}:
        raise ValueError("decision rule must be a joint all_of condition")
    by_metric = {}
    for c in conditions:
        c = _obj(c, "decision rule condition")
        if not isinstance(c.get("metric"), str):
            raise ValueError("decision rule condition metric must be a string")
        if c.get("metric") in by_metric:
            raise ValueError(f"duplicate decision condition {c.get('metric')}")
        by_metric[c.get("metric")] = c
    for metric, expected in EXPECTED_RULE.items():
        if metric not in by_metric:
            raise ValueError(f"decision rule must require {metric}")
        c = by_metric[metric]
        for field, value in expected.items():
            ok = _same_number(c.get(field), value) if isinstance(value, float) else c.get(field) == value
            if not ok:
                raise ValueError(f"decision rule {metric}.{field} must be {value!r}, got {c.get(field)!r}")
    if set(by_metric) != set(EXPECTED_RULE):
        raise ValueError("decision rule has unexpected conditions")


def _check_database(p):
    m = _obj(p.get("database_matrix"), "database_matrix")
    for field in ("scales_spans", "clients", "retained_versions"):
        _same_list(m.get(field), EXPECTED_MATRIX[field], f"database_matrix.{field}", _is_int)
    _same_list(m.get("filter_eligible_fraction"), EXPECTED_MATRIX["filter_eligible_fraction"],
               "database_matrix.filter_eligible_fraction", _is_number)
    if m.get("filter_correlation") != ["correlated", "uncorrelated"]:
        raise ValueError("database_matrix.filter_correlation must cover both cases")

    sched = _obj(p.get("initial_database_schedule"), "initial_database_schedule")
    for field in ("warmup_seconds", "measure_seconds", "runs"):
        _pos_int(sched.get(field), f"initial_database_schedule.{field}")

    seeds = p.get("seeds")
    if not isinstance(seeds, list) or not seeds or not all(_is_int(s) for s in seeds):
        raise ValueError("seeds must be a non-empty list of integers")
    if len(set(seeds)) != len(seeds):
        raise ValueError("seeds must be distinct")

    cache = _obj(p.get("cache_policy"), "cache_policy")
    for field in ("headline", "cold_start_procedure", "comparator_order"):
        _text(cache.get(field), f"cache_policy.{field}")
    timeout = _obj(p.get("timeout_accounting"), "timeout_accounting")
    if timeout.get("exclude_from_denominator") is not False:
        raise ValueError("timeout accounting must keep failures and timeouts in denominators")


def _check_scope(p):
    scope = _obj(p.get("claim_scope"), "claim_scope")
    _pos_int(scope.get("min_independent_source_groups_for_cross_corpus_claim"),
             "claim_scope.min_independent_source_groups_for_cross_corpus_claim")
    _text(scope.get("below_minimum"), "claim_scope.below_minimum")
    splits = _obj(p.get("question_splits"), "question_splits")
    if splits.get("splits") != ["development", "pilot", "final_holdout"]:
        raise ValueError("question_splits must be development, pilot, final_holdout")
    if splits.get("split_unit") != "source_group":
        raise ValueError("question splits must be by source group")


# ------------------------------------------------------- post-pilot (final)

def _check_post_pilot(p):
    pp = p.get("post_pilot")
    if not isinstance(pp, dict):
        raise ValueError("final stage requires a post_pilot object")
    unknown = set(pp) - set(POST_PILOT_KEYS)
    if unknown:
        raise ValueError(f"unknown post_pilot keys: {sorted(unknown)}")
    for key in POST_PILOT_REQUIRED:
        if key not in pp:
            raise ValueError(f"final protocol missing post_pilot.{key}")

    try:
        datetime.date.fromisoformat(pp["frozen_on"])
    except (TypeError, ValueError):
        raise ValueError("post_pilot.frozen_on must be an ISO date") from None
    _text(pp["pilot_evidence"], "post_pilot.pilot_evidence")
    if pp["frozen_before_holdout_measurement"] is not True:
        raise ValueError("post_pilot.frozen_before_holdout_measurement must be true")

    holdout = _obj(pp["final_holdout"], "post_pilot.final_holdout",
                   ("questions", "justification"))
    _pos_int(holdout.get("questions"), "post_pilot.final_holdout.questions")
    _text(holdout.get("justification"), "post_pilot.final_holdout.justification")

    ann = _obj(pp["ann_quality_target"], "post_pilot.ann_quality_target",
               ("metric", "k", "minimum"))
    if ann.get("metric") != "recall_at_k":
        raise ValueError("post_pilot.ann_quality_target.metric must be recall_at_k")
    _pos_int(ann.get("k"), "post_pilot.ann_quality_target.k")
    _finite(ann.get("minimum"), "post_pilot.ann_quality_target.minimum")
    if not 0 < ann["minimum"] <= 1:
        raise ValueError("post_pilot.ann_quality_target.minimum must be in (0, 1]")

    budgets = _obj(pp["performance_budgets"], "post_pilot.performance_budgets")
    if not budgets:
        raise ValueError("post_pilot.performance_budgets must name at least one budget")
    for name, b in budgets.items():
        where = f"post_pilot.performance_budgets.{name}"
        b = _obj(b, where, BUDGET_KEYS)
        for field in ("metric", "unit", "workload", "machine"):
            _text(b.get(field), f"{where}.{field}")
        if b.get("operator") not in BUDGET_OPERATORS:
            raise ValueError(f"{where}.operator must be one of {BUDGET_OPERATORS}")
        _finite(b.get("value"), f"{where}.value")
        if b["value"] <= 0:
            raise ValueError(f"{where}.value must be positive")

    sched = _obj(pp["database_schedule"], "post_pilot.database_schedule", SCHEDULE_KEYS)
    for field in SCHEDULE_KEYS:
        _pos_int(sched.get(field), f"post_pilot.database_schedule.{field}")

    narrowing = pp.get("matrix_narrowing", [])
    if not isinstance(narrowing, list):
        raise ValueError("post_pilot.matrix_narrowing must be a list")
    removed_by_dim = {}
    for item in narrowing:
        item = _obj(item, "post_pilot.matrix_narrowing item", NARROWING_KEYS)
        dim = item.get("dimension")
        if not isinstance(dim, str) or dim not in EXPECTED_MATRIX:
            raise ValueError(f"matrix_narrowing dimension {dim!r:.40} is not a frozen matrix dimension")
        removed = item.get("removed")
        if (not isinstance(removed, list) or not removed
                or any(not _is_number(v) or v not in EXPECTED_MATRIX[dim] for v in removed)):
            raise ValueError(f"matrix_narrowing.removed must list frozen {dim} values")
        _text(item.get("resource_limitation"), "matrix_narrowing.resource_limitation")
        removed_by_dim.setdefault(dim, set()).update(removed)
    for dim, removed in removed_by_dim.items():
        if removed >= set(EXPECTED_MATRIX[dim]):
            raise ValueError(f"matrix_narrowing cannot remove the entire {dim} dimension")

    agent = _obj(pp["agent_configuration"], "post_pilot.agent_configuration",
                 ("runner", "model", "budget_approval"))
    for field in ("runner", "model", "budget_approval"):
        _text(agent.get(field), f"post_pilot.agent_configuration.{field}")

    status = _obj(pp["external_comparator_status"], "post_pilot.external_comparator_status")
    for name in status:
        if name in REQUIRED_COMPARATORS:
            raise ValueError(f"required comparator {name} cannot be recorded as external")
        if name not in p["external_comparators"]:
            raise ValueError(f"external status for unlisted comparator {name}")
    for name, spec in p["external_comparators"].items():
        if name not in status:
            if spec["optional"]:
                continue
            raise ValueError(f"post_pilot.external_comparator_status missing {name}")
        s = _obj(status[name], f"external_comparator_status.{name}")
        if s.get("status") == "measured":
            _obj(s, f"external_comparator_status.{name}", ("status", "version"))
            _text(s.get("version"), f"external_comparator_status.{name}.version")
        elif s.get("status") == "unmeasured":
            _obj(s, f"external_comparator_status.{name}", ("status", "reason"))
            _text(s.get("reason"), f"external_comparator_status.{name}.reason")
        else:
            raise ValueError(f"external_comparator_status.{name}.status must be measured or unmeasured")


# ------------------------------------------------------------------ entry

def validate_protocol(protocol):
    """Raise ValueError unless ``protocol`` is structurally valid with unmodified
    Task 1 fields. For a final protocol this does not check its lock; see
    check_final_lock."""
    if not isinstance(protocol, dict):
        raise ValueError("protocol must be a JSON object")
    _check_comparators(protocol)
    unknown = set(protocol) - set(FROZEN_KEYS) - set(MUTABLE_KEYS)
    if unknown:
        raise ValueError(f"unknown protocol keys: {sorted(unknown)}")
    if protocol.get("schema") != "pg-evidence/benchmark-protocol":
        raise ValueError("schema must be pg-evidence/benchmark-protocol")
    if not _is_int(protocol.get("schema_version")) or protocol["schema_version"] != 1:
        raise ValueError("schema_version must be integer 1")
    stage = protocol.get("stage")
    if stage not in STAGES:
        raise ValueError(f"stage must be one of {STAGES}, got {stage!r}")

    _check_contrasts(protocol)
    _check_estimands_and_rule(protocol)
    _check_database(protocol)
    _check_scope(protocol)

    digest = frozen_digest(protocol)
    if digest != FROZEN_SHA256:
        raise ValueError(f"frozen protocol modified: digest {digest} != {FROZEN_SHA256}")

    if stage == "pre_pilot":
        if protocol.get("post_pilot") is not None:
            raise ValueError("a pre_pilot protocol cannot contain post_pilot values")
    else:
        _check_post_pilot(protocol)


def check_final_lock(protocol, lock):
    """Raise ValueError unless ``lock`` is a well-formed final lock whose digest
    equals the digest of the whole ``protocol``."""
    lock = _obj(lock, "final lock", ("schema", "protocol_sha256"))
    if lock.get("schema") != LOCK_SCHEMA:
        raise ValueError(f"final lock schema must be {LOCK_SCHEMA}")
    expected = lock.get("protocol_sha256")
    if (not isinstance(expected, str) or len(expected) != 64
            or any(ch not in "0123456789abcdef" for ch in expected)):
        raise ValueError("final lock protocol_sha256 must be 64 lowercase hex digits")
    actual = protocol_digest(protocol)
    if actual != expected:
        raise ValueError(f"protocol digest {actual} does not match final lock {expected}")


def _reject_constant(name):
    raise ValueError(f"non-finite JSON constant {name} is not allowed")


def _finite_float(text):
    value = float(text)
    if not math.isfinite(value):
        raise ValueError(f"non-finite JSON number {text} is not allowed")
    return value


def _no_duplicates(pairs):
    seen = {}
    for key, value in pairs:
        if key in seen:
            raise ValueError(f"duplicate JSON key {key!r}")
        seen[key] = value
    return seen


def load_protocol(path):
    """Strict JSON load: no NaN/Infinity, overflowing floats or duplicate keys.
    Also used for final lock files."""
    with open(path, encoding="utf-8") as f:
        return json.load(f, parse_constant=_reject_constant, parse_float=_finite_float,
                         object_pairs_hook=_no_duplicates)


def main(argv=None):
    parser = argparse.ArgumentParser(prog="python -m bench.protocol")
    parser.add_argument("--check", metavar="PATH", required=True,
                        help="validate a protocol JSON file")
    parser.add_argument("--final-lock", metavar="LOCK",
                        help="committed lock file; required for a final protocol")
    args = parser.parse_args(argv)
    try:
        protocol = load_protocol(args.check)
        validate_protocol(protocol)
        if protocol["stage"] == "final":
            if args.final_lock is None:
                raise ValueError("a final protocol requires --final-lock; "
                                 "structural validity is not a locked final freeze")
            check_final_lock(protocol, load_protocol(args.final_lock))
        elif args.final_lock is not None:
            raise ValueError("--final-lock applies only to a final protocol")
    except (OSError, ValueError, RecursionError) as exc:
        print(f"invalid protocol {args.check}: {exc}", file=sys.stderr)
        return 1
    locked = f" protocol_sha256={protocol_digest(protocol)} (matches final lock)" \
        if protocol["stage"] == "final" else ""
    print(f"valid protocol {args.check}: stage={protocol['stage']} "
          f"frozen_sha256={frozen_digest(protocol)}{locked}")
    return 0


if __name__ == "__main__":
    sys.exit(main())
