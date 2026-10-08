# Vector compression experiment

Status: research candidate, not enabled in 0.1.0. The implemented corpus uses
pgvector `vector(N)` and an HNSW `vector_cosine_ops` index. Citation source bytes,
span bytes and their identities are independent of vector representation.

[TurboQuant](https://arxiv.org/abs/2504.19874) studies online low-bit vector
quantization. Applying it here requires a versioned encoding, a distance scorer,
and an index access path that actually consumes the compressed representation.
Putting opaque compressed bytes in a column does not make pgvector HNSW use them.
Published KV-cache results are not evidence of PostgreSQL retrieval performance.

## Comparison to run

Freeze one embedding dataset, held-out queries, exact cosine ground truth and
filter/history workloads before tuning. Compare:

1. Current float32 vectors and HNSW, plus exhaustive cosine as recall ground truth.
2. pgvector half-precision indexing.
3. pgvector binary-quantized indexing followed by original-vector reranking.
4. A separately reviewed TurboQuant prototype at declared bit rates, with and
   without original-vector reranking.

The native pgvector alternatives are documented in its
[version 0.8.7 README](https://github.com/pgvector/pgvector/blob/v0.8.7/README.md#half-precision-indexing).
They are experiment arms, not currently exposed collection options.

Report recall@k at a fixed latency target and latency at fixed recall, p50/p95,
index build time, insertion cost, PostgreSQL relation/index bytes, and peak memory
under the same concurrency. Count rotation/codebook metadata, graph edges,
retained originals, and reranking buffers. Separate compressed vector payload
savings from total database and process-memory savings. Include selective filters,
stale versions and queries outside the ingest distribution.

A prototype must preserve dump/restore, cancellation, grants, corruption detection
and citation IDs. Encoding changes need schema migration and format-version tests.
Keep this experiment on a separate branch; do not change the frozen agent evaluator
or add an unreviewed quantization dependency to the initial package.
