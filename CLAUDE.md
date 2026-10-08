# pg-evidence contributor instructions

Read README.md, docs/roadmap.md and the contract sections needed for your task.
This is a PostgreSQL extension in development; packaging/probe is only a toolchain
probe. Do not present it as the product or invent API/benchmark results.

Use bounded tasks and focused diffs. Write behavior tests first. Run host checks
from CONTRIBUTING.md and real PostgreSQL tests when database behavior changes.
Backend tests must be compiled into the extension crate. Preserve source bytes,
version IDs, permissions and documented retry semantics. Qualify dynamic SQL names
and parameterize values. Never run model inference inside the backend.

Keep the evaluator fixed during optimization; follow docs/autoresearch.md. Preserve
failed runs. No final-holdout tuning, automatic merges, unbudgeted model spending or
release publication as a side effect of a code task. The license is undecided.
