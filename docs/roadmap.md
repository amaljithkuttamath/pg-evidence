# Roadmap and release gates

| Area | Current evidence | Remaining work |
|---|---|---|
| Native product | ARM Linux build, release installation and fresh Docker runtime pass | Native x86-64 CI activation and verification |
| Evidence lifecycle | Versioned UTF-8 bytes, retries, conflict checks, retire/purge, old citations and dump/restore pass | Broader recovery and failure-injection schedules |
| Retrieval and composition | Literal, regex, lexical, vector, tags, one-hop links and bounded plans implemented | Filter/history ANN recall and active operator cancellation coverage |
| Permissions | Invoker functions, real reader/writer/purger grants and direct-edit denial tested | Wider deployment/security review |
| Benchmarks | Independent SQL schema/content comparisons and a 1,000-span paired smoke run | Fully matched budgets, controlled latency/memory, scale and live-agent trials |
| Distribution | Install archive and runtime image built and checked locally; source on GitHub | License decision and public binary/container release |

The 0.1.0 developer preview is functional. Its [verification record](evidence/2026-10-08-arm64/README.md)
identifies exactly what passed and what is still unproven. It is not a production
readiness or speed claim.

[Vector compression](vector-compression.md), including TurboQuant, is a separate
measured experiment. It must preserve citation bytes, restore behavior and declared
recall before becoming a supported collection option.

A failed or inconclusive token-efficiency target may accompany an honest functional
release. Claiming a 25% reduction requires the predeclared token and answer-quality
conditions to both pass. See the [benchmark protocol](benchmark-method.md). The
license remains undecided; no release tag has been published.
