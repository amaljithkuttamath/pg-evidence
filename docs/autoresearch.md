# Autoresearch

Two tracks support the project:

1. **Upstream research:** follow relevant papers and PostgreSQL, pgrx, pgvector,
   pg_search and pgvectorscale changes. Record primary sources, dates, what was
   learned and a specific implementation or benchmark implication.
2. **Benchmark experiments:** run bounded candidate changes against an unchanged
   evaluator and baseline. Preserve failures and propose useful changes through
   reviewed pull requests.

The experiment pattern is inspired by [autoresearch](https://github.com/karpathy/autoresearch):
keep evaluation fixed while changing a limited implementation surface. Its language
model training workload is not a database benchmark.

## Rules

- Start from a recorded commit, dataset manifest, environment and random seeds.
- Check correctness before comparing performance. A broken citation or permission
  invariant rejects a candidate regardless of speed or token count.
- Keep benchmark thresholds, fixtures and evaluator outside the candidate's edit
  scope. Changes to them require a separate reviewed protocol revision.
- Compare against the matched SQL implementation, not only the previous candidate.
- Use development data for optimization. Do not tune against the final holdout.
- Bound each run by candidate count and wall time; retain timeout and failure records.
- Send improvements as pull requests with the hypothesis, diff, commands, raw
  results and limitations. Do not automatically merge or publish releases.
- Public CI runners can verify builds and correctness. Their timing results are
  exploratory unless the benchmark protocol establishes a suitable controlled host.

## Current availability

The build and protocol checks are executable. The product, matched SQL baseline
and performance harness are still planned. There is therefore no valid product
optimization result yet. Research can inform those implementations immediately;
benchmark experiments begin when the corresponding executable baseline exists.

The project's coordinator runs the reasoning and opens GitHub proposals; Actions
provides reproducible checks. No model credential is stored in this repository and
no paid model/API evaluation is enabled without a budget decision.
