"""Protocol freeze tests (Task 1, decisions C11 and C12).

Standard library only. Values in FINAL_FIXTURE are test inputs used to exercise
the final-stage validator; they are not protocol decisions or targets.
"""

import copy
import json
import math
import os
import subprocess
import sys
import tempfile
import unittest

from bench.protocol import protocol_digest, validate_protocol

ROOT = os.path.dirname(os.path.dirname(os.path.dirname(os.path.abspath(__file__))))
PROTOCOL_PATH = os.path.join(ROOT, "bench", "protocol.json")

# Test-only values. They demonstrate shape, not chosen sizes or budgets.
FINAL_FIXTURE = {
    "frozen_on": "2099-01-01",
    "pilot_evidence": "test-fixture/pilot-report",
    "frozen_before_holdout_measurement": True,
    "final_holdout": {"questions": 7, "justification": "test fixture"},
    "ann_quality_target": {"metric": "recall_at_k", "k": 3, "minimum": 0.5},
    "performance_budgets": {
        "fixture_budget": {
            "metric": "p95_latency",
            "operator": "<=",
            "value": 1.5,
            "unit": "ms",
            "workload": "test fixture",
            "machine": "test fixture",
        }
    },
    "database_schedule": {"warmup_seconds": 1, "measure_seconds": 2, "runs": 3},
    "matrix_narrowing": [],
    "agent_configuration": {
        "runner": "test fixture",
        "model": "test fixture",
        "budget_approval": "test fixture",
    },
    "external_comparator_status": {
        "paradedb_pg_search": {"status": "unmeasured", "reason": "test fixture"},
        "pgvectorscale_diskann": {"status": "measured", "version": "0.0.0-test"},
    },
}


def load_committed():
    with open(PROTOCOL_PATH, encoding="utf-8") as f:
        return json.load(f)


def final_protocol():
    p = load_committed()
    p["stage"] = "final"
    p["post_pilot"] = copy.deepcopy(FINAL_FIXTURE)
    return p


def run_cli(path, *extra):
    return subprocess.run(
        [sys.executable, "-E", "-s", "-m", "bench.protocol", "--check", path, *extra],
        cwd=ROOT,
        capture_output=True,
        text=True,
        timeout=60,
    )


class PlanExampleTests(unittest.TestCase):
    def test_requires_matched_sql(self):
        with self.assertRaises(ValueError):
            validate_protocol({"comparators": ["extension_direct"]})


class CommittedProtocolTests(unittest.TestCase):
    def test_committed_protocol_is_valid_pre_pilot(self):
        p = load_committed()
        self.assertEqual(p["stage"], "pre_pilot")
        self.assertIsNone(p["post_pilot"])
        validate_protocol(p)

    def test_committed_protocol_has_no_post_pilot_values(self):
        # C12: holdout size, ANN target and budgets are not set before the pilot.
        def keys(node):
            if isinstance(node, dict):
                for k, v in node.items():
                    yield k
                    yield from keys(v)
            elif isinstance(node, list):
                for v in node:
                    yield from keys(v)

        found = set(keys(load_committed()))
        for key in ("final_holdout", "ann_quality_target", "performance_budgets"):
            self.assertNotIn(key, found)

    def test_committed_thresholds(self):
        rule = load_committed()["decision_rule"]["composition_claim"]["all_of"]
        by_metric = {c["metric"]: c for c in rule}
        tok = by_metric["token_ratio"]
        self.assertEqual((tok["bound"], tok["operator"], tok["threshold"]), ("upper", "<=", 0.75))
        q = by_metric["supported_answer_rate_difference"]
        self.assertEqual((q["bound"], q["operator"], q["threshold"]), ("lower", ">", -0.03))
        for c in rule:
            self.assertEqual((c["sided"], c["confidence"]), ("one", 0.95))

    def test_final_fixture_is_accepted(self):
        validate_protocol(final_protocol())


class FrozenFieldTests(unittest.TestCase):
    def assertRejected(self, p, pattern=None):
        if pattern is None:
            with self.assertRaises(ValueError):
                validate_protocol(p)
        else:
            with self.assertRaisesRegex(ValueError, pattern):
                validate_protocol(p)

    def test_each_required_comparator(self):
        for name in load_committed()["comparators"]:
            with self.subTest(name=name):
                p = load_committed()
                p["comparators"].remove(name)
                self.assertRejected(p, "missing required comparator")

    def test_duplicate_or_non_string_comparators(self):
        p = load_committed()
        p["comparators"].append(p["comparators"][0])
        self.assertRejected(p, "duplicate")
        p = load_committed()
        p["comparators"].append(1)
        self.assertRejected(p)
        p = load_committed()
        p["comparators"] = "sql_direct"
        self.assertRejected(p)

    def test_external_comparators_must_be_listed(self):
        for name in ("paradedb_pg_search", "pgvectorscale_diskann"):
            with self.subTest(name=name):
                p = load_committed()
                del p["external_comparators"][name]
                self.assertRejected(p, "external comparison must be listed")

    def test_listed_external_cannot_be_marked_optional(self):
        p = load_committed()
        p["external_comparators"]["paradedb_pg_search"]["optional"] = True
        self.assertRejected(p, "paradedb_pg_search")

    def test_required_comparator_cannot_be_external(self):
        p = load_committed()
        p["external_comparators"]["pgvector_hnsw"] = {"optional": True}
        self.assertRejected(p, "required comparator")

    def test_primary_contrasts(self):
        cases = {
            "composition_tokens": ["extension_direct", "extension_composed"],  # swapped
            "extension_overhead": ["sql_composed", "extension_composed"],
        }
        for key, bad in cases.items():
            with self.subTest(key=key):
                p = load_committed()
                p["primary_contrasts"][key] = bad
                self.assertRejected(p)
        p = load_committed()
        p["primary_contrasts"]["composition_tokens"] = ["files_ripgrep", "extension_direct"]
        self.assertRejected(p, "E-C versus E-D")

    def test_replication_and_workflow_roles(self):
        p = load_committed()
        p["replication_contrasts"]["composition_replication"] = ["sql_direct", "sql_composed"]
        self.assertRejected(p, "S-C versus S-D")
        p = load_committed()
        p["workflow_comparisons"]["files_ripgrep"]["attributes_savings_to_extension"] = True
        self.assertRejected(p, "files_ripgrep")
        p = load_committed()
        p["secondary_contrasts"]["claims"] = True
        self.assertRejected(p, "secondary")

    def test_threshold_mutations(self):
        bad_values = [0.8, 0.7, True, False, "0.75", None, math.nan, math.inf, 1]
        for value in bad_values:
            with self.subTest(value=value):
                p = load_committed()
                rule = p["decision_rule"]["composition_claim"]["all_of"]
                next(c for c in rule if c["metric"] == "token_ratio")["threshold"] = value
                self.assertRejected(p)
        for value in [-0.05, 0.0, True, "-0.03", math.nan, -math.inf]:
            with self.subTest(value=value):
                p = load_committed()
                rule = p["decision_rule"]["composition_claim"]["all_of"]
                next(c for c in rule if c["metric"] == "supported_answer_rate_difference")["threshold"] = value
                self.assertRejected(p)

    def test_sidedness_and_confidence(self):
        mutations = [("sided", "two"), ("confidence", 0.9), ("confidence", True),
                     ("confidence", math.nan), ("bound", "lower"), ("operator", "<")]
        for field, value in mutations:
            with self.subTest(field=field, value=value):
                p = load_committed()
                rule = p["decision_rule"]["composition_claim"]["all_of"]
                next(c for c in rule if c["metric"] == "token_ratio")[field] = value
                self.assertRejected(p)

    def test_decision_rule_requires_both_conditions(self):
        p = load_committed()
        rule = p["decision_rule"]["composition_claim"]
        rule["all_of"] = [c for c in rule["all_of"] if c["metric"] == "token_ratio"]
        self.assertRejected(p, "supported_answer_rate_difference")
        p = load_committed()
        p["decision_rule"]["composition_claim"]["any_of"] = p["decision_rule"]["composition_claim"].pop("all_of")
        self.assertRejected(p)

    def test_estimand_denominator(self):
        p = load_committed()
        p["estimands"]["token_ratio"]["denominator"] = "successful_questions"
        self.assertRejected(p, "attempted")
        p = load_committed()
        p["estimands"]["supported_answer_rate_difference"]["failed_attempts"] = "excluded"
        self.assertRejected(p)

    def test_database_matrix(self):
        mutations = [
            ("scales_spans", [100000]),
            ("scales_spans", [100000, 1000000, True]),
            ("clients", [1, 8]),
            ("clients", [1.0, 8, 32]),
            ("retained_versions", [1, 5]),
            ("filter_eligible_fraction", [1.0, 0.1, 0.01]),
            ("filter_eligible_fraction", [1.0, 0.1, 0.01, math.nan]),
        ]
        for field, value in mutations:
            with self.subTest(field=field, value=value):
                p = load_committed()
                p["database_matrix"][field] = value
                self.assertRejected(p)

    def test_initial_schedule(self):
        for field, value in [("warmup_seconds", 0), ("measure_seconds", True),
                             ("runs", 2.5), ("runs", "5")]:
            with self.subTest(field=field, value=value):
                p = load_committed()
                p["initial_database_schedule"][field] = value
                self.assertRejected(p)

    def test_timeout_accounting_and_cache_policy(self):
        p = load_committed()
        p["timeout_accounting"]["exclude_from_denominator"] = True
        self.assertRejected(p, "timeout")
        p = load_committed()
        p["cache_policy"]["cold_start_procedure"] = ""
        self.assertRejected(p)

    def test_seeds(self):
        for value in [[], [1, 1], [1, True], [1.5], "1"]:
            with self.subTest(value=value):
                p = load_committed()
                p["seeds"] = value
                self.assertRejected(p)

    def test_cross_corpus_minimum(self):
        for value in [0, True, 2.0, None, "3"]:
            with self.subTest(value=value):
                p = load_committed()
                p["claim_scope"]["min_independent_source_groups_for_cross_corpus_claim"] = value
                self.assertRejected(p)

    def test_semantically_valid_edit_still_breaks_freeze(self):
        p = load_committed()
        p["seeds"] = list(reversed(p["seeds"]))
        self.assertRejected(p, "frozen protocol modified")
        p = load_committed()
        p["database_matrix"]["notes"] = p["database_matrix"].get("notes", "") + " edited"
        self.assertRejected(p, "frozen protocol modified")

    def test_unknown_keys_and_types(self):
        p = load_committed()
        p["post_pilot_extra"] = 1
        self.assertRejected(p, "unknown")
        self.assertRejected([], "object")
        p = load_committed()
        p["stage"] = "pilot"
        self.assertRejected(p, "stage")
        p = load_committed()
        p["schema_version"] = True
        self.assertRejected(p)


class StagedFreezeTests(unittest.TestCase):
    def test_pre_pilot_rejects_post_pilot_values(self):
        p = load_committed()
        p["post_pilot"] = copy.deepcopy(FINAL_FIXTURE)
        with self.assertRaisesRegex(ValueError, "pre_pilot"):
            validate_protocol(p)

    def test_final_requires_post_pilot(self):
        p = load_committed()
        p["stage"] = "final"
        with self.assertRaisesRegex(ValueError, "post_pilot"):
            validate_protocol(p)

    def test_final_missing_each_field(self):
        for key in FINAL_FIXTURE:
            if key == "matrix_narrowing":
                continue
            with self.subTest(key=key):
                p = final_protocol()
                del p["post_pilot"][key]
                with self.assertRaisesRegex(ValueError, key):
                    validate_protocol(p)

    def test_final_holdout_size(self):
        for value in [0, -1, True, 1.5, "100", None, math.nan]:
            with self.subTest(value=value):
                p = final_protocol()
                p["post_pilot"]["final_holdout"]["questions"] = value
                with self.assertRaises(ValueError):
                    validate_protocol(p)
        p = final_protocol()
        p["post_pilot"]["final_holdout"]["justification"] = " "
        with self.assertRaises(ValueError):
            validate_protocol(p)

    def test_ann_target(self):
        for field, value in [("minimum", math.nan), ("minimum", math.inf), ("minimum", 1.2),
                             ("minimum", 0), ("minimum", True), ("minimum", "0.95"),
                             ("k", 0), ("k", True), ("k", 10.0), ("metric", "ndcg")]:
            with self.subTest(field=field, value=value):
                p = final_protocol()
                p["post_pilot"]["ann_quality_target"][field] = value
                with self.assertRaises(ValueError):
                    validate_protocol(p)

    def test_performance_budgets(self):
        p = final_protocol()
        p["post_pilot"]["performance_budgets"] = {}
        with self.assertRaises(ValueError):
            validate_protocol(p)
        for field, value in [("value", math.nan), ("value", -1), ("value", True),
                             ("value", "1"), ("operator", "~"), ("machine", ""),
                             ("workload", None)]:
            with self.subTest(field=field, value=value):
                p = final_protocol()
                p["post_pilot"]["performance_budgets"]["fixture_budget"][field] = value
                with self.assertRaises(ValueError):
                    validate_protocol(p)

    def test_freeze_before_holdout(self):
        for value in [False, "true", 1]:
            with self.subTest(value=value):
                p = final_protocol()
                p["post_pilot"]["frozen_before_holdout_measurement"] = value
                with self.assertRaises(ValueError):
                    validate_protocol(p)
        p = final_protocol()
        p["post_pilot"]["frozen_on"] = "yesterday"
        with self.assertRaises(ValueError):
            validate_protocol(p)

    def test_final_schedule_types(self):
        for field, value in [("runs", 0), ("runs", True), ("measure_seconds", math.nan)]:
            with self.subTest(field=field, value=value):
                p = final_protocol()
                p["post_pilot"]["database_schedule"][field] = value
                with self.assertRaises(ValueError):
                    validate_protocol(p)

    def test_external_status(self):
        p = final_protocol()
        p["post_pilot"]["external_comparator_status"]["paradedb_pg_search"]["reason"] = ""
        with self.assertRaisesRegex(ValueError, "reason"):
            validate_protocol(p)
        p = final_protocol()
        del p["post_pilot"]["external_comparator_status"]["pgvectorscale_diskann"]
        with self.assertRaisesRegex(ValueError, "pgvectorscale_diskann"):
            validate_protocol(p)
        p = final_protocol()
        p["post_pilot"]["external_comparator_status"]["sql_direct"] = {
            "status": "unmeasured", "reason": "x"}
        with self.assertRaisesRegex(ValueError, "required comparator"):
            validate_protocol(p)
        p = final_protocol()
        p["post_pilot"]["external_comparator_status"]["pgvectorscale_diskann"] = {
            "status": "measured"}
        with self.assertRaisesRegex(ValueError, "version"):
            validate_protocol(p)
        p = final_protocol()
        p["post_pilot"]["external_comparator_status"]["paradedb_pg_search"]["status"] = "vendor_reported"
        with self.assertRaises(ValueError):
            validate_protocol(p)

    def test_matrix_narrowing_needs_resource_limitation(self):
        p = final_protocol()
        p["post_pilot"]["matrix_narrowing"] = [
            {"dimension": "clients", "removed": [32], "resource_limitation": ""}]
        with self.assertRaisesRegex(ValueError, "resource_limitation"):
            validate_protocol(p)
        p = final_protocol()
        p["post_pilot"]["matrix_narrowing"] = [
            {"dimension": "clients", "removed": [64], "resource_limitation": "x"}]
        with self.assertRaisesRegex(ValueError, "removed"):
            validate_protocol(p)
        p = final_protocol()
        p["post_pilot"]["matrix_narrowing"] = [
            {"dimension": "clients", "removed": [32], "resource_limitation": "host has 12 CPUs"}]
        validate_protocol(p)

    def test_agent_configuration(self):
        for field in ("runner", "model", "budget_approval"):
            with self.subTest(field=field):
                p = final_protocol()
                p["post_pilot"]["agent_configuration"][field] = ""
                with self.assertRaisesRegex(ValueError, field):
                    validate_protocol(p)

    def test_unknown_post_pilot_key(self):
        p = final_protocol()
        p["post_pilot"]["token_target"] = 0.5
        with self.assertRaisesRegex(ValueError, "unknown"):
            validate_protocol(p)


class CliTests(unittest.TestCase):
    def setUp(self):
        self.tmp = tempfile.TemporaryDirectory()
        self.addCleanup(self.tmp.cleanup)

    def write(self, text):
        path = os.path.join(self.tmp.name, "protocol.json")
        with open(path, "w", encoding="utf-8") as f:
            f.write(text)
        return path

    def assertCliRejects(self, path, needle=None):
        r = run_cli(path)
        self.assertNotEqual(r.returncode, 0, r.stdout + r.stderr)
        self.assertNotIn("Traceback", r.stderr)
        if needle:
            self.assertIn(needle, r.stderr)

    def test_committed_file_passes(self):
        r = run_cli(PROTOCOL_PATH)
        self.assertEqual(r.returncode, 0, r.stderr)
        self.assertIn("pre_pilot", r.stdout)

    def write_lock(self, digest, **extra):
        path = os.path.join(self.tmp.name, "final-lock.json")
        lock = {"schema": "pg-evidence/final-protocol-lock", "protocol_sha256": digest}
        lock.update(extra)
        with open(path, "w", encoding="utf-8") as f:
            json.dump(lock, f)
        return path

    def test_final_fixture_with_matching_lock_passes(self):
        p = final_protocol()
        r = run_cli(self.write(json.dumps(p)), "--final-lock", self.write_lock(protocol_digest(p)))
        self.assertEqual(r.returncode, 0, r.stderr)
        self.assertIn("final", r.stdout)

    def test_final_without_lock_fails(self):
        # Structurally valid, but a final run needs its separately committed lock.
        self.assertCliRejects(self.write(json.dumps(final_protocol())), "--final-lock")

    def test_well_formed_final_mutation_fails_against_same_lock(self):
        lock = self.write_lock(protocol_digest(final_protocol()))
        mutations = {
            "holdout": lambda pp: pp["final_holdout"].__setitem__("questions", 8),
            "ann": lambda pp: pp["ann_quality_target"].__setitem__("minimum", 0.6),
            "budget": lambda pp: pp["performance_budgets"]["fixture_budget"].__setitem__("value", 9.0),
        }
        for name, mutate in mutations.items():
            with self.subTest(name=name):
                p = final_protocol()
                mutate(p["post_pilot"])
                validate_protocol(p)  # still structurally valid
                r = run_cli(self.write(json.dumps(p)), "--final-lock", lock)
                self.assertNotEqual(r.returncode, 0)
                self.assertIn("does not match final lock", r.stderr)
                self.assertNotIn("Traceback", r.stderr)

    def test_malformed_locks_fail(self):
        p = final_protocol()
        path = self.write(json.dumps(p))
        good = protocol_digest(p)
        bad_locks = [
            self.write_lock(good.upper()),
            self.write_lock(good[:-1]),
            self.write_lock(good, extra=1),
            self.write_lock(None),
            os.path.join(self.tmp.name, "absent-lock.json"),
        ]
        for lock in bad_locks:
            with self.subTest(lock=lock):
                r = run_cli(path, "--final-lock", lock)
                self.assertNotEqual(r.returncode, 0)
                self.assertNotIn("Traceback", r.stderr)

    def test_pre_pilot_rejects_final_lock(self):
        p = load_committed()
        r = run_cli(PROTOCOL_PATH, "--final-lock", self.write_lock(protocol_digest(p)))
        self.assertNotEqual(r.returncode, 0)
        self.assertNotIn("Traceback", r.stderr)

    def test_overflowing_float_literal_fails(self):
        text = json.dumps(final_protocol()).replace('"minimum": 0.5', '"minimum": 1e400')
        self.assertIn("1e400", text)
        self.assertCliRejects(self.write(text), "non-finite")

    def test_giant_integer_fails_cleanly(self):
        p = final_protocol()
        p["post_pilot"]["performance_budgets"]["fixture_budget"]["value"] = 10 ** 400
        self.assertCliRejects(self.write(json.dumps(p)), "value")

    def test_mutated_threshold_fails(self):
        p = load_committed()
        rule = p["decision_rule"]["composition_claim"]["all_of"]
        next(c for c in rule if c["metric"] == "token_ratio")["threshold"] = 0.8
        self.assertCliRejects(self.write(json.dumps(p)), "token_ratio")

    def test_semantically_valid_mutation_fails(self):
        p = load_committed()
        p["seeds"] = list(reversed(p["seeds"]))
        self.assertCliRejects(self.write(json.dumps(p)), "frozen protocol modified")

    def test_incomplete_final_fails(self):
        p = final_protocol()
        del p["post_pilot"]["performance_budgets"]
        self.assertCliRejects(self.write(json.dumps(p)), "performance_budgets")

    def test_nan_literal_fails(self):
        p = final_protocol()
        text = json.dumps(p).replace('"minimum": 0.5', '"minimum": NaN')
        self.assertIn("NaN", text)
        self.assertCliRejects(self.write(text), "NaN")

    def test_duplicate_key_fails(self):
        text = json.dumps(load_committed())
        text = text.replace('"stage": "pre_pilot"', '"stage": "final", "stage": "pre_pilot"', 1)
        self.assertCliRejects(self.write(text), "duplicate")

    def test_malformed_and_missing_files_fail(self):
        self.assertCliRejects(self.write("{not json"))
        self.assertCliRejects(os.path.join(self.tmp.name, "absent.json"))

    def test_usage_error(self):
        r = subprocess.run([sys.executable, "-E", "-s", "-m", "bench.protocol"], cwd=ROOT,
                           capture_output=True, text=True, timeout=60)
        self.assertNotEqual(r.returncode, 0)


BAD_VALUES = [None, [], {}, True, 10 ** 400, -1, "x", [[]], [{}]]


def leaf_paths(node, prefix=()):
    yield prefix
    if isinstance(node, dict):
        for k, v in node.items():
            yield from leaf_paths(v, prefix + (k,))
    elif isinstance(node, list):
        for i, v in enumerate(node):
            yield from leaf_paths(v, prefix + (i,))


def replaced(protocol, path, value):
    p = copy.deepcopy(protocol)
    node = p
    for key in path[:-1]:
        node = node[key]
    node[path[-1]] = value
    return p


class MalformedTypeTests(unittest.TestCase):
    """validate_protocol raises only ValueError, whatever type a field holds."""

    def test_review_examples(self):
        p = final_protocol()
        p["decision_rule"]["composition_claim"]["all_of"][0]["metric"] = []
        with self.assertRaises(ValueError):
            validate_protocol(p)
        for value in (None, {}, "x", [None], [[]]):
            with self.subTest(matrix_narrowing=value):
                p = final_protocol()
                p["post_pilot"]["matrix_narrowing"] = value
                with self.assertRaises(ValueError):
                    validate_protocol(p)

    def test_giant_integers(self):
        cases = [("final_holdout", "questions"), ("ann_quality_target", "minimum"),
                 ("ann_quality_target", "k"), ("database_schedule", "runs")]
        for section, field in cases:
            with self.subTest(field=field):
                p = final_protocol()
                p["post_pilot"][section][field] = 10 ** 400
                with self.assertRaises(ValueError):
                    validate_protocol(p)
        p = final_protocol()
        p["post_pilot"]["performance_budgets"]["fixture_budget"]["value"] = 10 ** 400
        with self.assertRaises(ValueError):
            validate_protocol(p)

    def test_unknown_nested_post_pilot_keys(self):
        targets = [("final_holdout",), ("ann_quality_target",),
                   ("performance_budgets", "fixture_budget"), ("database_schedule",),
                   ("agent_configuration",),
                   ("external_comparator_status", "paradedb_pg_search")]
        for path in targets:
            with self.subTest(path=path):
                p = final_protocol()
                node = p["post_pilot"]
                for key in path:
                    node = node[key]
                node["unexpected"] = 1
                with self.assertRaisesRegex(ValueError, "unknown"):
                    validate_protocol(p)
        p = final_protocol()
        p["post_pilot"]["matrix_narrowing"] = [
            {"dimension": "clients", "removed": [32], "resource_limitation": "x", "note": 1}]
        with self.assertRaisesRegex(ValueError, "unknown"):
            validate_protocol(p)

    def test_narrowing_cannot_remove_whole_dimension(self):
        p = final_protocol()
        p["post_pilot"]["matrix_narrowing"] = [
            {"dimension": "clients", "removed": [1, 8, 32], "resource_limitation": "x"}]
        with self.assertRaisesRegex(ValueError, "entire"):
            validate_protocol(p)
        p = final_protocol()
        p["post_pilot"]["matrix_narrowing"] = [
            {"dimension": "scales_spans", "removed": [100000], "resource_limitation": "x"},
            {"dimension": "scales_spans", "removed": [1000000], "resource_limitation": "y"}]
        with self.assertRaisesRegex(ValueError, "entire"):
            validate_protocol(p)

    def test_every_field_with_every_bad_type(self):
        for base in (final_protocol(), load_committed()):
            for path in leaf_paths(base):
                if not path:
                    continue
                for value in BAD_VALUES:
                    p = replaced(base, path, value)
                    try:
                        validate_protocol(p)
                    except ValueError:
                        pass
                    except Exception as exc:  # noqa: BLE001 - the point of the test
                        self.fail(f"{path} = {value!r:.40} raised {type(exc).__name__}: {exc}")


if __name__ == "__main__":
    unittest.main()
