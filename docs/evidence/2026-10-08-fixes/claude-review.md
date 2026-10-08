**Verdict: the approach is sound.** I found no blocking correctness regressions in the diff. I did a static review only and ran nothing, so the native build result and the actual test results are still unknown.

## Lifecycle lock fix (`src/ops.rs`)

**Row locks and stale snapshots: correct.**
- The `UPDATE` always writes a new row version, even though `annotation_revision` is set to its own value. When the holder commits, a REPEATABLE READ or SERIALIZABLE waiter that tries to update the same row gets `40001`. This holds whether the waiter was already blocked or arrives after the commit, because its snapshot cannot see the new row version.
- READ COMMITTED waiters re-check the `WHERE` subquery against the newest row and continue. Their later statements take fresh snapshots and see the tombstone or embeddings. That is the same as before.
- Both sides call the same helper, so each order (holder first or waiter first) produces a real write.
- Updating the row a second time later in the same transaction (publish's own revision bump) is fine.

**Lock strength is now weaker.** An `UPDATE` of a non-key column takes `FOR NO KEY UPDATE`, not `FOR UPDATE`. Foreign-key checks take `FOR KEY SHARE`, which no longer waits behind attach, publish or purge. If any write path inserts or deletes child rows that reference `assets` and relies on attach, publish or purge holding the asset lock without locking the asset row itself, the two can now run at the same time. If every such path locks or updates the asset row, nothing changes. Please confirm this; I can't see the other call sites.

**Grants: one thing to check.** `SET annotation_revision = a.annotation_revision` reads the column, so it needs `SELECT(annotation_revision)` as well as `UPDATE(annotation_revision)`. `RETURNING` needs `SELECT(asset_id)`, as before. If the writer and purger have table-level `SELECT`, this is fine. If the grants are column-level and leave out `annotation_revision`, the call fails with `42501`. Row-level security policies on `assets`, if any, now apply as an `UPDATE` policy. Also confirm that no other caller of `lock_asset_of_version` runs under a role with fewer privileges.

**Retries: more `40001`s, expected and documented.** Under REPEATABLE READ, any transaction that later locks or updates the asset row now gets `40001` after a concurrent attach, publish or purge on any version of that asset. Before, a lock-only conflict let it continue. This covers `stage_version`, annotation updates, two attaches to different versions, and identical idempotent retries. `docs/design.md` covers it, which fits G6. Idempotent calls now also write the row and generate WAL. That is a cost, not a defect.

**Error code reaching the client: uncertain.** This depends on `write_json`. If it catches SPI errors and turns them into an `ApiError` with a different SQLSTATE, the `40001` is lost. The new tests would catch that, so it is only a concern until they run.

## Concurrency tests

- They should catch the original bugs. Before the fix, a REPEATABLE READ or SERIALIZABLE waiter commits (publish or attach case) or leaves embeddings behind (purge case), and the `rc != 0` and embedding-count assertions fail. The waiter's snapshot is taken before the writer commits, as required.
- The SERIALIZABLE/SERIALIZABLE case may already fail with `40001` before the fix through SSI, so it may not tell the old and new behavior apart. It does no harm.
- Writers running at REPEATABLE READ or SERIALIZABLE are only covered when the waiter is also SERIALIZABLE. That is a small gap, not a defect.
- The `'0/0'` revision check assumes that staging and purge never bump `content_revision` or `annotation_revision`. If either does, this assertion fails for a reason unrelated to the fix.
- `parse_error(err)` has to handle a plain PostgreSQL error, which has no product `reason`. Make sure it doesn't throw when there is no JSON detail.

## Importer (`examples/import_files.py`)

**`chunk_spans` is correct for valid UTF-8 with `max_bytes >= 4`:**
- Backing off over continuation bytes keeps `cut > start`.
- A newline is always a character boundary, so `newline + 1` is a valid cut.
- An empty `rfind` window returns -1, so every span is non-empty.
- Every chunk except the last is at least `0.9·max − 3` bytes long. For a 1 MiB file that is at most about 583 spans, around 55 KB at roughly 95 bytes per span, under 65536. The margin depends on the per-span response size staying near 95 bytes; the stage preflight is the backstop.

**Small defect:** a `--max-span-bytes` value below 4 now ends in a `ValueError` traceback rather than a clean argparse error. Check it in `main()` and call `ap.error(...)`.

**Host test (`ChunkSpansTest`):**
- It catches the old behavior: the old line-aligned packing gives 1001-byte spans, which fails the `>= 1800` assertion. The `(0, 1900), (1900, 2100)` check tests the 10% window boundary.
- `test_examples_system.py` must import `unittest`, `json` and `tempfile`. These lines aren't in the diff; if `unittest` is missing, the whole module fails to import.
- Importing `import_files` when the module loads runs its top-level imports. If it imports a database driver that the host environment lacks, the pure-Python test fails to load. It also has to keep `main()` behind `if __name__ == '__main__'`.
- If the host run skips or filters out system test modules, this test lives in one and may never run on the host. Check the runner configuration.

**System test:** it catches the old failure (1047 spans and a 97535-byte response against the default 65536), as long as `--init` creates the collection with the default response limit. The file is 1,048,047 bytes, under the 1 MiB source limit.

**Docs:** both doc edits match the new behavior.
