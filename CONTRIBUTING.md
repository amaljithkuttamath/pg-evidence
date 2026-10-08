# Contributing

Start with the [roadmap](docs/roadmap.md) and [contract](docs/design.md).
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

Run `packaging/run-product.sh` for database changes; it builds the actual
extension and exercises backend, multi-session and restore behavior. See
[build details](docs/build-environment.md). The older toolchain probe is retained
for toolchain investigations, including its intentional negative layout test.

Performance proposals need a baseline, controlled inputs, retained raw results and
correctness checks. Do not change the evaluator and optimized implementation in the
same experiment. Do not claim token or memory improvements from unmeasured examples.
