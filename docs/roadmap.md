# Roadmap and release gates

| Milestone | Acceptance condition | Status |
|---|---|---|
| Development foundation | Pinned Rust/PG18 probe, real database tests, validated benchmark protocol | ARM verified; native x86 CI being established |
| Evidence lifecycle | Publish A, publish B, resolve A's citation exactly; retry safely; restore with IDs and permissions intact | Planned |
| Retrieval | Literal, lexical, vector and filtered queries with explicit budgets and recall accounting | Planned |
| Composition | One-hop links, tags and bounded plans using one consistent snapshot | Planned |
| Release evidence | Correctness/recovery suite, matched SQL comparisons, live agent evaluation and clean package install | Planned |

The first public package will contain the real extension, installation instructions,
source checksums and reproducible evidence. GitHub Releases will carry versioned
assets. A container package is useful only once it provides the working extension.
License selection remains pending; no release tag is reserved or published.

A failed or inconclusive token-efficiency target may accompany an honest functional
release. Claiming a 25% reduction requires the predeclared token and answer-quality
conditions to both pass. See the [benchmark protocol](benchmark-method.md).
