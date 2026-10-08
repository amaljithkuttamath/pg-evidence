"""Validate an exported probe evidence directory (packaging/probe-steps.sh output).

Usage: python3 -I packaging/check_probe.py OUT_DIR
Exit 0 only if every mandatory step succeeded with positive evidence in its log
and the G2 tests/ experiment reproduced its recorded negative outcome. Exit 1
otherwise. A step exit code of 0 alone is not accepted: a cargo test filter that
matches no tests also exits 0. Standard library only.
"""

import os
import re
import sys

KNOWN_STEPS = ("lockfile", "versions", "g2_src", "g2_tests_dir", "install", "load", "checksums")
MANDATORY = ("versions", "g2_src", "install", "load", "checksums")
SRC_TESTS = ("src_pg_test_runs_in_backend", "src_pg_test_with_pgvector")
TESTS_DIR_TEST = "tests_dir_pg_test_runs_in_backend"
ANSI = re.compile(r"\x1b\[[0-9;]*m")
SECTION = re.compile(r"^\s*(?:Running (unittests \S+|tests/\S+)|Doc-tests \S+)", re.M)


class ProbeError(Exception):
    pass


def read(out, name):
    path = os.path.join(out, name)
    try:
        with open(path, encoding="utf-8", errors="replace") as f:
            return ANSI.sub("", f.read())
    except OSError as exc:
        raise ProbeError(f"cannot read {name}: {exc.strerror}") from None


def parse_results(out):
    steps, completed = {}, False
    for n, line in enumerate(read(out, "probe-results.tsv").splitlines(), 1):
        if completed:
            raise ProbeError("probe-results.tsv has unexpected data after probe completion")
        if re.fullmatch(r"probe-steps exit \d+", line):
            if line != "probe-steps exit 0":
                raise ProbeError(f"unsuccessful probe completion: {line}")
            completed = True
            continue
        m = re.fullmatch(r"([a-z0-9_]+)\t(\d+)\t\d+s", line)
        if not m or m.group(1) not in KNOWN_STEPS:
            raise ProbeError(f"probe-results.tsv line {n} is malformed: {line!r:.80}")
        if m.group(1) in steps:
            raise ProbeError(f"probe-results.tsv has a duplicate step {m.group(1)}")
        steps[m.group(1)] = int(m.group(2))
    if not completed:
        raise ProbeError("probe-steps.sh did not complete (no 'probe-steps exit' line)")
    return steps


def section(log, target):
    """Text of the cargo test section whose header names `target`."""
    starts = list(SECTION.finditer(log))
    for i, m in enumerate(starts):
        if m.group(1) and m.group(1).startswith(target):
            end = starts[i + 1].start() if i + 1 < len(starts) else len(log)
            return log[m.end():end]
    return ""


def passed_count(text):
    m = re.search(r"test result: ok\. (\d+) passed; 0 failed", text)
    return int(m.group(1)) if m else 0


def check_g2_src(out):
    text = section(read(out, "step-g2_src.log"), "unittests src/lib.rs")
    missing = [t for t in SRC_TESTS
               if not re.search(rf"^test tests::(?:pg_)?{t} \.\.\. ok$", text, re.M)]
    if missing:
        raise ProbeError(f"g2_src: named test(s) did not pass: {', '.join(missing)}")
    if passed_count(text) < len(SRC_TESTS):
        raise ProbeError("g2_src: src/lib.rs test result does not report the named tests passing")
    if "Finished installing pg_evidence_probe" not in text:
        raise ProbeError("g2_src: no evidence the extension was installed into the test cluster")
    return f"{passed_count(text)} src/ #[pg_test] passed in a PostgreSQL backend"


def check_install(out):
    if "Finished installing pg_evidence_probe" not in read(out, "step-install.log"):
        raise ProbeError("install: no 'Finished installing pg_evidence_probe'")
    return "release build installed"


def check_load(out):
    log = read(out, "step-load.log")
    required = (r"server_version=18\.\S*.*", r"cosine_distance=1",
                r"extension vector=\S+", r"extension pg_evidence_probe=\S+")
    missing = [p for p in required if not re.search(rf"^{p}$", log, re.M)]
    if missing:
        raise ProbeError(f"load: missing expected output {missing}")
    return "pg_evidence_probe and vector loaded; SPI call into pgvector returned 1"


def check_checksums(out):
    lines = re.findall(r"^[0-9a-f]{64}  \S+$", read(out, "step-checksums.log"), re.M)
    wanted = ("pg_evidence_probe.so", ".sql", "pg_evidence_probe.control")
    if len(lines) != 3 or not all(any(l.endswith(w) for l in lines) for w in wanted):
        raise ProbeError("checksums: expected SHA-256 for the .so, .sql and .control files")
    return "3 installed files hashed"


def check_versions(out):
    read(out, "step-versions.log")
    return "versions captured"


def check_g2_tests_dir(out, rc):
    """The recorded G2 result is negative: tests/ ran one test whose SQL function
    is absent. Anything else (a pass, a build failure, zero tests) differs."""
    log = read(out, "step-g2_tests_dir.log")
    text = section(log, "tests/g2_tests_dir.rs")
    observed = (rc != 0 and re.search(r"^running 1 test$", text, re.M)
                and re.search(rf"^test tests::(?:pg_)?{TESTS_DIR_TEST} \.\.\. FAILED$", text, re.M)
                and f"function tests.{TESTS_DIR_TEST}() does not exist" in log)
    if not observed:
        raise ProbeError("g2_tests_dir: outcome differs from the recorded negative G2 result "
                         "(1 test run, SQL function absent); review before relying on it")
    return "expected negative observed: tests/ #[pg_test] ran, its SQL function is absent"


def main(argv):
    if len(argv) != 2:
        print("usage: check_probe.py OUT_DIR", file=sys.stderr)
        return 2
    out = argv[1]
    try:
        steps = parse_results(out)
    except ProbeError as exc:
        print(f"FAIL {exc}")
        return 1
    failures = 0
    if steps.get("lockfile", 0) != 0:
        print(f"FAIL lockfile: exit {steps['lockfile']}")
        failures += 1
    checks = {"versions": check_versions, "g2_src": check_g2_src, "install": check_install,
              "load": check_load, "checksums": check_checksums}
    for name in MANDATORY:
        if name not in steps:
            print(f"FAIL {name}: step missing from probe-results.tsv")
            failures += 1
        elif steps[name] != 0:
            print(f"FAIL {name}: exit {steps[name]}")
            failures += 1
        else:
            try:
                print(f"PASS {name}: {checks[name](out)}")
            except ProbeError as exc:
                print(f"FAIL {exc}")
                failures += 1
    try:
        if "g2_tests_dir" not in steps:
            raise ProbeError("g2_tests_dir: step missing from probe-results.tsv")
        print(f"G2 g2_tests_dir: {check_g2_tests_dir(out, steps['g2_tests_dir'])}")
    except ProbeError as exc:
        print(f"FAIL {exc}")
        failures += 1
    print("probe evidence: " + ("ACCEPTED" if not failures else f"REJECTED ({failures} problem(s))"))
    return 1 if failures else 0


if __name__ == "__main__":
    sys.exit(main(sys.argv))
