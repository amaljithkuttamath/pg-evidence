# Contributing

Start with the [roadmap](docs/roadmap.md) and [proposed contract](docs/design.md).
The project is experimental and its license is undecided. A product release is
not available yet.

Keep changes focused. Write behavior tests before implementation, run the affected
suite, and include commands and outcomes in the pull request. Test database behavior
against PostgreSQL, including real roles and concurrent sessions where relevant.
Do not substitute mocked results for backend verification.

```sh
python3 -m unittest discover -s bench/tests -p 'test_*.py'
python3 -m unittest discover -s packaging/tests -p 'test_*.py'
python3 -m bench.protocol --check bench/protocol.json
```

Use [the Docker probe](docs/build-environment.md) for toolchain changes. The negative
integration-test experiment is intentional and must remain explicitly reported.

Performance proposals need a baseline, controlled inputs, retained raw results and
correctness checks. Do not change the evaluator and optimized implementation in the
same experiment. Do not claim token or memory improvements from unmeasured examples.
