"""Tests for packaging/check_probe.py and packaging/run-probe.sh (no Docker).

Fixtures are synthetic probe exports shaped like the real ones. run-probe.sh is
exercised with fake `docker` and `df` executables placed first on PATH.
"""

import os
import shutil
import stat
import subprocess
import sys
import tempfile
import textwrap
import unittest

PACKAGING = os.path.dirname(os.path.dirname(os.path.abspath(__file__)))
CHECKER = os.path.join(PACKAGING, "check_probe.py")
RUNNER = os.path.join(PACKAGING, "run-probe.sh")
ARTIFACTS = os.path.join(PACKAGING, "artifacts")

SRC_LOG = """\
     Running unittests src/lib.rs (/work/target/debug/deps/pg_evidence_probe-1)

running 2 tests
    Finished installing pg_evidence_probe
test tests::pg_src_pg_test_runs_in_backend ... ok
test tests::pg_src_pg_test_with_pgvector ... ok

test result: ok. 2 passed; 0 failed; 0 ignored; 0 measured; 0 filtered out; finished in 16.87s

     Running tests/g2_tests_dir.rs (/work/target/debug/deps/g2_tests_dir-1)

running 0 tests

test result: ok. 0 passed; 0 failed; 0 ignored; 0 measured; 1 filtered out; finished in 0.00s
"""

TESTS_DIR_LOG = """\
     Running unittests src/lib.rs (/work/target/debug/deps/pg_evidence_probe-1)

running 0 tests

test result: ok. 0 passed; 0 failed; 0 ignored; 0 measured; 2 filtered out; finished in 0.00s

     Running tests/g2_tests_dir.rs (/work/target/debug/deps/g2_tests_dir-1)

running 1 test
test tests::pg_tests_dir_pg_test_runs_in_backend ... FAILED
ERROR:  function tests.tests_dir_pg_test_runs_in_backend() does not exist at character 8

test result: FAILED. 0 passed; 1 failed; 0 ignored; 0 measured; 0 filtered out; finished in 3.00s
"""

LOAD_LOG = """\
CREATE EXTENSION
CREATE EXTENSION
server_version=18.6 (Debian 18.6-1.pgdg13+2)
cosine_distance=1
extension pg_evidence_probe=0.0.0
extension plpgsql=1.0
extension vector=0.8.7
encoding=UTF8
"""

CHECKSUMS_LOG = """\
{h}  /usr/lib/postgresql/18/lib/pg_evidence_probe.so
{h}  /usr/share/postgresql/18/extension/pg_evidence_probe--0.0.0.sql
{h}  /usr/share/postgresql/18/extension/pg_evidence_probe.control
""".format(h="a" * 64)

PASSING_RESULTS = [("versions", 0), ("g2_src", 0), ("g2_tests_dir", 1),
                   ("install", 0), ("load", 0), ("checksums", 0)]


def write_fixture(path, results=PASSING_RESULTS, logs=None, completed=True):
    os.makedirs(path, exist_ok=True)
    files = {"versions": "aarch64\n", "g2_src": SRC_LOG, "g2_tests_dir": TESTS_DIR_LOG,
             "install": "Finished installing pg_evidence_probe\n", "load": LOAD_LOG,
             "checksums": CHECKSUMS_LOG}
    files.update(logs or {})
    for name, text in files.items():
        if text is not None:
            with open(os.path.join(path, f"step-{name}.log"), "w") as f:
                f.write(text)
    with open(os.path.join(path, "probe-results.tsv"), "w") as f:
        for name, rc in results:
            f.write(f"{name}\t{rc}\t1s\n")
        if completed:
            f.write("probe-steps exit 0\n")
    return path


def check(path):
    return subprocess.run([sys.executable, "-I", CHECKER, path],
                          capture_output=True, text=True, timeout=60)


class CheckerTests(unittest.TestCase):
    def setUp(self):
        self.tmp = tempfile.mkdtemp()
        self.addCleanup(shutil.rmtree, self.tmp)

    def fixture(self, **kw):
        return write_fixture(os.path.join(self.tmp, "out"), **kw)

    def assertFails(self, path, needle):
        r = check(path)
        self.assertEqual(r.returncode, 1, r.stdout + r.stderr)
        self.assertIn(needle, r.stdout + r.stderr)
        self.assertNotIn("Traceback", r.stderr)

    def test_passing_fixture(self):
        r = check(self.fixture())
        self.assertEqual(r.returncode, 0, r.stdout + r.stderr)
        self.assertIn("g2_tests_dir: expected negative observed", r.stdout)
        self.assertNotIn("g2_tests_dir: passed", r.stdout)

    def test_mandatory_step_failed(self):
        for step in ("versions", "g2_src", "install", "load", "checksums"):
            with self.subTest(step=step):
                results = [(n, 1 if n == step else rc) for n, rc in PASSING_RESULTS]
                self.assertFails(write_fixture(os.path.join(self.tmp, step), results), step)

    def test_mandatory_step_missing(self):
        results = [(n, rc) for n, rc in PASSING_RESULTS if n != "install"]
        self.assertFails(self.fixture(results=results), "install")

    def test_incomplete_run(self):
        self.assertFails(self.fixture(completed=False), "did not complete")

    def test_failed_or_repeated_completion_is_not_a_pass(self):
        for suffix in ("probe-steps exit 2\n", "probe-steps exit 0\nprobe-steps exit 0\n"):
            with self.subTest(suffix=suffix):
                path = self.fixture()
                result = os.path.join(path, "probe-results.tsv")
                with open(result) as f:
                    text = f.read().replace("probe-steps exit 0\n", suffix)
                with open(result, "w") as f:
                    f.write(text)
                self.assertFails(path, "completion")

    def test_failed_lockfile_step(self):
        self.assertFails(self.fixture(results=[("lockfile", 101)] + PASSING_RESULTS), "lockfile")

    def test_zero_src_tests_is_not_a_pass(self):
        log = SRC_LOG.replace("running 2 tests", "running 0 tests").replace(
            "test result: ok. 2 passed; 0 failed; 0 ignored; 0 measured; 0 filtered out",
            "test result: ok. 0 passed; 0 failed; 0 ignored; 0 measured; 2 filtered out")
        log = "\n".join(l for l in log.splitlines() if not l.startswith("test tests::"))
        self.assertFails(self.fixture(logs={"g2_src": log}), "g2_src")

    def test_named_src_test_missing(self):
        log = SRC_LOG.replace("test tests::pg_src_pg_test_with_pgvector ... ok\n", "")
        self.assertFails(self.fixture(logs={"g2_src": log}), "src_pg_test_with_pgvector")

    def test_load_without_backend_evidence(self):
        for line in ("cosine_distance=1", "extension vector=", "extension pg_evidence_probe=",
                     "server_version=18."):
            with self.subTest(line=line):
                log = "\n".join(l for l in LOAD_LOG.splitlines() if not l.startswith(line))
                path = write_fixture(os.path.join(self.tmp, line[:8]), logs={"load": log})
                self.assertFails(path, "load")

    def test_checksums_incomplete(self):
        log = "\n".join(CHECKSUMS_LOG.splitlines()[:2]) + "\n"
        self.assertFails(self.fixture(logs={"checksums": log}), "checksums")

    def test_g2_tests_dir_unexpected_pass(self):
        results = [(n, 0 if n == "g2_tests_dir" else rc) for n, rc in PASSING_RESULTS]
        self.assertFails(self.fixture(results=results), "differs from the recorded")

    def test_g2_tests_dir_build_failure_is_inconclusive(self):
        log = "error: failed to run custom build command for `pgrx-pg-sys v0.19.3`\n"
        self.assertFails(self.fixture(logs={"g2_tests_dir": log}), "differs from the recorded")

    def test_malformed_results(self):
        path = self.fixture()
        with open(os.path.join(path, "probe-results.tsv"), "a") as f:
            f.write("g2_src\tzero\t1s\n")
        self.assertFails(path, "probe-results.tsv")

    def test_duplicate_step(self):
        self.assertFails(self.fixture(results=PASSING_RESULTS + [("load", 0)]), "duplicate")

    def test_missing_directory_or_log(self):
        self.assertFails(os.path.join(self.tmp, "absent"), "probe-results.tsv")
        self.assertFails(self.fixture(logs={"load": None}), "step-load.log")

    def test_ansi_colour_codes_are_ignored(self):
        log = SRC_LOG.replace("test result: ok.", "\x1b[32mtest result: ok.\x1b[0m")
        self.assertEqual(check(self.fixture(logs={"g2_src": log})).returncode, 0)


@unittest.skipUnless(os.path.isdir(os.path.join(ARTIFACTS, "probe-arm64-5", "out")),
                     "local probe evidence not present (packaging/artifacts is gitignored)")
class RecordedEvidenceTests(unittest.TestCase):
    def test_attempts_4_and_5_pass(self):
        for n in (4, 5):
            with self.subTest(attempt=n):
                r = check(os.path.join(ARTIFACTS, f"probe-arm64-{n}", "out"))
                self.assertEqual(r.returncode, 0, r.stdout + r.stderr)

    def test_attempt_3_fails(self):
        r = check(os.path.join(ARTIFACTS, "probe-arm64-3", "out"))
        self.assertEqual(r.returncode, 1, r.stdout + r.stderr)


FAKE_DOCKER = """\
#!/usr/bin/env bash
echo "$*" >> "$FAKE_LOG"
sp=""
trap 'echo TERM >> "$FAKE_LOG"; [ -n "$sp" ] && kill "$sp"; exit 143' TERM
dest=""
for arg in "$@"; do
    case "$arg" in type=local,dest=*) dest="${arg#type=local,dest=}" ;; esac
done
if [ -n "${FAKE_SLEEP:-}" ]; then sleep "$FAKE_SLEEP" & sp=$!; wait "$sp"; fi
if [ -n "${FAKE_FIXTURE:-}" ] && [ -n "$dest" ]; then
    mkdir -p "$dest"; cp -R "$FAKE_FIXTURE"/. "$dest"/
fi
exit "${FAKE_RC:-0}"
"""

# Pops one value per call from $FAKE_DF_SEQ, repeating the last.
FAKE_DF = """\
#!/usr/bin/env bash
v="$(head -n 1 "$FAKE_DF_SEQ")"
if [ "$(wc -l < "$FAKE_DF_SEQ")" -gt 1 ]; then
    tail -n +2 "$FAKE_DF_SEQ" > "$FAKE_DF_SEQ.tmp" && mv "$FAKE_DF_SEQ.tmp" "$FAKE_DF_SEQ"
fi
echo "Filesystem 1024-blocks Used Available Capacity Mounted on"
echo "fake 100 0 $v 0% /"
"""


class RunnerTests(unittest.TestCase):
    def setUp(self):
        self.tmp = tempfile.mkdtemp()
        self.addCleanup(shutil.rmtree, self.tmp)
        self.bin = os.path.join(self.tmp, "bin")
        os.mkdir(self.bin)
        for name, text in (("docker", FAKE_DOCKER), ("df", FAKE_DF)):
            path = os.path.join(self.bin, name)
            with open(path, "w") as f:
                f.write(text)
            os.chmod(path, os.stat(path).st_mode | stat.S_IXUSR)
        self.log = os.path.join(self.tmp, "docker.log")
        self.out = os.path.join(self.tmp, "export")

    def run_probe(self, df_values, **env):
        seq = os.path.join(self.tmp, "df-seq")
        with open(seq, "w") as f:
            f.write("".join(f"{v}\n" for v in df_values))
        full_env = dict(os.environ, PATH=self.bin + os.pathsep + os.environ["PATH"],
                        FAKE_LOG=self.log, FAKE_DF_SEQ=seq, MIN_FREE_KB="1000",
                        GUARD_INTERVAL="0.2", **env)
        return subprocess.run(["bash", RUNNER, "linux/arm64", self.out], env=full_env,
                              capture_output=True, text=True, timeout=60)

    def docker_calls(self):
        if not os.path.exists(self.log):
            return ""
        with open(self.log) as f:
            return f.read()

    def test_low_space_preflight_never_starts_docker(self):
        r = self.run_probe([10])
        self.assertEqual(r.returncode, 3, r.stderr)
        self.assertIn("free space", r.stderr)
        self.assertEqual(self.docker_calls(), "")

    def test_passing_export_succeeds(self):
        fixture = write_fixture(os.path.join(self.tmp, "fixture"))
        r = self.run_probe([5000], FAKE_FIXTURE=fixture)
        self.assertEqual(r.returncode, 0, r.stdout + r.stderr)
        self.assertIn("buildx build", self.docker_calls())

    def test_failed_probe_export_fails_after_preserving_logs(self):
        results = [(n, 1 if n == "load" else rc) for n, rc in PASSING_RESULTS]
        fixture = write_fixture(os.path.join(self.tmp, "fixture"), results)
        r = self.run_probe([5000], FAKE_FIXTURE=fixture)
        self.assertEqual(r.returncode, 1, r.stdout + r.stderr)
        self.assertTrue(os.path.exists(os.path.join(self.out, "step-load.log")))

    def test_build_failure_propagates(self):
        r = self.run_probe([5000], FAKE_RC="17")
        self.assertEqual(r.returncode, 17, r.stdout + r.stderr)

    def test_low_space_during_build_cancels_with_exit_3(self):
        r = self.run_probe([5000, 5000, 10], FAKE_SLEEP="20")
        self.assertEqual(r.returncode, 3, r.stdout + r.stderr)
        self.assertIn("cancelling", r.stderr)
        self.assertIn("TERM", self.docker_calls())


if __name__ == "__main__":
    unittest.main()
