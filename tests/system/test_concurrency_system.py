"""Multi-session tests (G4, G6, G9 subsets). Interleavings are forced with an
advisory lock held by a controller session and observed in pg_stat_activity,
not with sleeps. Each session is a separate psql process."""
import json
import subprocess
import time
import unittest

from pgev import (SystemTestCase, env_for, lit, new_id, parse_error, psql_args,
                  stage_request)

LOCK_KEY = 7340001


class Session:
    """A psql process fed a script on stdin; output collected on finish()."""

    def __init__(self, db, app, script, keep_open=False):
        self.app = app
        self.proc = subprocess.Popen(psql_args(), stdin=subprocess.PIPE, stdout=subprocess.PIPE,
                                     stderr=subprocess.PIPE, text=True,
                                     env=env_for(db.name, PGAPPNAME=app))
        self.proc.stdin.write(script)
        self.proc.stdin.flush()
        if not keep_open:
            self.proc.stdin.close()

    def send(self, script):
        self.proc.stdin.write(script)
        self.proc.stdin.flush()

    def finish(self, timeout=60):
        if self.proc.stdin is not None:
            if not self.proc.stdin.closed:
                self.proc.stdin.close()
            self.proc.stdin = None
        out, err = self.proc.communicate(timeout=timeout)
        return self.proc.returncode, out, err


class ConcurrencyTest(SystemTestCase):
    def lifecycle_race(self, writer_op, waiter_op):
        self.db.run('CREATE EXTENSION vector;')
        isolations = [('READ COMMITTED', 'READ COMMITTED'),
                      ('READ COMMITTED', 'REPEATABLE READ'),
                      ('READ COMMITTED', 'SERIALIZABLE'),
                      ('SERIALIZABLE', 'SERIALIZABLE')]
        for i, (writer_isolation, waiter_isolation) in enumerate(isolations):
            with self.subTest(writer=writer_isolation, waiter=waiter_isolation):
                corpus = f'race_{i}'
                self.db.init(corpus, {'embedding_model': 'm', 'embedding_dimensions': 3})
                staged = self.db.stage(corpus, new_id(), 'a.txt', 'hello', [(0, 5)], 'seed', 0)
                version = staged['version_id']
                payloads = {
                    'attach_embeddings': {'version_id': version, 'model': 'm', 'embeddings': [
                        {'evidence_id': staged['spans'][0]['evidence_id'], 'vector': [1, 0, 0]}]},
                    'purge': {'version_id': version, 'reason': 'regression'},
                    'publish_version': {'version_id': version},
                }
                if waiter_op == 'publish_version':
                    self.db.api('attach_embeddings', corpus, payloads['attach_embeddings'])
                holder = self.hold_lock()
                writer = Session(self.db, 'pgev_race_writer',
                                 f'BEGIN ISOLATION LEVEL {writer_isolation};\n' +
                                 self.db.api_sql(writer_op, corpus, payloads[writer_op]) +
                                 f'\nSELECT pg_advisory_lock({LOCK_KEY});\nCOMMIT;\n')
                self.wait_blocked('pgev_race_writer', 'advisory')
                waiter = Session(self.db, 'pgev_race_waiter',
                                 f'BEGIN ISOLATION LEVEL {waiter_isolation};\n' +
                                 self.db.api_sql(waiter_op, corpus, payloads[waiter_op]) + '\nCOMMIT;\n')
                self.wait_blocked('pgev_race_waiter')
                self.release(holder)
                wr, _, we = writer.finish()
                rc, _, err = waiter.finish()
                self.assertEqual(wr, 0, we)
                if waiter_isolation != 'READ COMMITTED':
                    self.assertNotEqual(rc, 0, 'fixed-snapshot waiter incorrectly committed')
                    self.assertEqual(parse_error(err).sqlstate, '40001', err)
                    # A fresh transaction can retry safely after a serialization failure.
                    if waiter_op == 'purge':
                        self.db.api(waiter_op, corpus, payloads[waiter_op])
                    else:
                        retry = self.db.api_error(waiter_op, corpus, payloads[waiter_op])
                        self.assertEqual((retry.sqlstate, retry.reason), ('55000', 'version_purged'))
                elif waiter_op == 'purge':
                    self.assertEqual(rc, 0, err)
                else:
                    self.assertNotEqual(rc, 0)
                    error = parse_error(err)
                    self.assertEqual((error.sqlstate, error.reason), ('55000', 'version_purged'))
                self.assertEqual(self.db.scalar(
                    f'SELECT count(*) FROM {corpus}.assets a JOIN {corpus}.tombstones t '
                    'ON t.version_id = a.current_version_id;'), '0')
                self.assertEqual(self.db.scalar(f'SELECT count(*) FROM {corpus}.embeddings;'), '0')
                # Lock coordination must not increment either public revision counter.
                self.assertEqual(self.db.scalar(
                    f'SELECT content_revision::text || \'/\' || annotation_revision FROM {corpus}.assets;'), '0/0')

    def test_purge_cannot_race_publish_across_isolation_levels(self):
        self.lifecycle_race('purge', 'publish_version')

    def test_purge_cannot_race_attach_across_isolation_levels(self):
        self.lifecycle_race('purge', 'attach_embeddings')

    def test_attach_cannot_escape_concurrent_purge(self):
        self.lifecycle_race('attach_embeddings', 'purge')

    def wait_for(self, sql, what, timeout=30):
        deadline = time.monotonic() + timeout
        while time.monotonic() < deadline:
            if self.db.scalar(sql) == 't':
                return
            time.sleep(0.05)
        self.fail(f'timed out waiting for {what}')

    def wait_blocked(self, app, event=None):
        cond = f"wait_event = {lit(event)}" if event else 'true'
        self.wait_for(
            f"SELECT EXISTS (SELECT 1 FROM pg_stat_activity WHERE application_name = {lit(app)} "
            f"AND wait_event_type = 'Lock' AND {cond});", f'{app} to block')

    def hold_lock(self):
        holder = Session(self.db, 'pgev_holder', f'SELECT pg_advisory_lock({LOCK_KEY});\n', keep_open=True)
        self.wait_for(f"SELECT EXISTS (SELECT 1 FROM pg_locks l JOIN pg_stat_activity a USING (pid) "
                      f"WHERE a.application_name = 'pgev_holder' AND l.locktype = 'advisory' AND l.granted);",
                      'controller lock')
        self.addCleanup(lambda: holder.proc.poll() is None and holder.finish())
        return holder

    def release(self, holder):
        holder.send(f'SELECT pg_advisory_unlock({LOCK_KEY});\n')
        holder.finish()

    def seed(self):
        self.db.init('docs')
        asset = new_id()
        a = self.db.stage('docs', asset, 'a.md', 'version one', [(0, 7)], 'k0', 0)
        self.db.publish('docs', a['version_id'])
        return asset

    def call(self, func, request):
        return self.db.api_sql(func, 'docs', request)

    def test_two_publishers_one_wins_other_gets_revision_conflict(self):
        asset = self.seed()
        b1 = self.db.stage('docs', asset, 'a.md', 'version two', [(0, 7)], 'k1', 1)
        b2 = self.db.stage('docs', asset, 'a.md', 'version 2b', [(0, 7)], 'k2', 1)
        holder = self.hold_lock()
        s1 = Session(self.db, 'pgev_s1', f"BEGIN;\n{self.call('publish_version', {'version_id': b1['version_id']})}\n"
                                         f"SELECT pg_advisory_lock({LOCK_KEY});\nCOMMIT;\n")
        self.wait_blocked('pgev_s1', 'advisory')
        s2 = Session(self.db, 'pgev_s2', self.call('publish_version', {'version_id': b2['version_id']}) + '\n')
        self.wait_blocked('pgev_s2')  # waits on the asset row lock held by s1
        self.release(holder)
        rc1, out1, err1 = s1.finish()
        rc2, _, err2 = s2.finish()
        self.assertEqual(rc1, 0, err1)
        self.assertEqual(json.loads(out1.strip().split('\n')[0])['revision'], 2)
        err = parse_error(err2)
        self.assertNotEqual(rc2, 0)
        self.assertEqual((err.sqlstate, err.reason), ('55000', 'revision_conflict'))
        self.assertEqual(self.db.scalar('SELECT content_revision FROM docs.assets;'), '2')

    def concurrent_identical_stage(self, isolation):
        self.db.init('docs')
        req = stage_request(new_id(), 'n.md', 'new asset text', [(0, 3)], 'same-key', 0)
        holder = self.hold_lock()
        s1 = Session(self.db, 'pgev_s1', f"BEGIN;\n{self.call('stage_version', req)}\n"
                                         f"SELECT pg_advisory_lock({LOCK_KEY});\nCOMMIT;\n")
        self.wait_blocked('pgev_s1', 'advisory')
        s2 = Session(self.db, 'pgev_s2', f"SET default_transaction_isolation = {lit(isolation)};\n"
                                         f"{self.call('stage_version', req)}\n")
        self.wait_blocked('pgev_s2')  # waits on s1's uncommitted asset row
        self.release(holder)
        rc1, out1, err1 = s1.finish()
        rc2, out2, err2 = s2.finish()
        self.assertEqual(rc1, 0, err1)
        first = json.loads(out1.strip().split('\n')[0])
        self.assertEqual(self.db.scalar('SELECT count(*) FROM docs.versions;'), '1')
        return first, rc2, out2, err2

    def test_concurrent_identical_stage_converges_at_read_committed(self):
        first, rc2, out2, err2 = self.concurrent_identical_stage('read committed')
        self.assertEqual(rc2, 0, err2)
        second = json.loads(out2.strip().split('\n')[-1])
        self.assertTrue(second['replayed'])
        self.assertEqual(second['version_id'], first['version_id'])
        self.assertEqual(second['spans'], first['spans'])

    def test_concurrent_identical_stage_at_repeatable_read(self):
        # Contract (G6): converge, or a genuine 40001 that is safe to retry.
        first, rc2, out2, err2 = self.concurrent_identical_stage('repeatable read')
        if rc2 == 0:
            self.assertEqual(json.loads(out2.strip().split('\n')[-1])['version_id'], first['version_id'])
            print('\nG6 observed at REPEATABLE READ: replayed stored IDs')
        else:
            self.assertEqual(parse_error(err2).sqlstate, '40001', err2)
            print('\nG6 observed at REPEATABLE READ: 40001 serialization failure')
        retry = self.db.api('stage_version', 'docs', stage_request(
            first['asset_id'], 'n.md', 'new asset text', [(0, 3)], 'same-key', 0))
        self.assertEqual(retry['version_id'], first['version_id'])

    def test_tagging_and_publishing_do_not_conflict(self):
        asset = self.seed()
        b = self.db.stage('docs', asset, 'a.md', 'version two', [(0, 7)], 'k1', 1)
        holder = self.hold_lock()
        tag = {'action': 'tag', 'asset_id': asset, 'tags': ['reviewed'], 'expected_annotation_revision': 0}
        s1 = Session(self.db, 'pgev_s1', f"BEGIN;\n{self.call('annotate', tag)}\n"
                                         f"SELECT pg_advisory_lock({LOCK_KEY});\nCOMMIT;\n")
        self.wait_blocked('pgev_s1', 'advisory')
        s2 = Session(self.db, 'pgev_s2', self.call('publish_version', {'version_id': b['version_id']}) + '\n')
        self.wait_blocked('pgev_s2')
        self.release(holder)
        self.assertEqual(s1.finish()[0], 0)
        rc2, out2, err2 = s2.finish()
        self.assertEqual(rc2, 0, err2)
        self.assertEqual(json.loads(out2.strip())['revision'], 2)
        self.assertEqual(self.db.scalar('SELECT annotation_revision::text || content_revision FROM docs.assets;'), '12')

    def test_query_never_mixes_versions_during_publishes(self):
        db = self.db
        db.init('docs')
        asset = new_id()

        def doc(i):
            return f'token alpha {i:04d} token beta {i:04d}'

        v = db.stage('docs', asset, 'mix.md', doc(0), [(0, 16), (17, 32)], 'k0', 0)
        db.publish('docs', v['version_id'])
        rounds = 40
        script = []
        for i in range(1, rounds + 1):
            req = stage_request(asset, 'mix.md', doc(i), [(0, 16), (17, 32)], f'k{i}', i)
            script.append(
                f"SELECT evidence.stage_version('docs', {lit(json.dumps(req))}::jsonb)->>'version_id' AS vid \\gset\n"
                f"SELECT evidence.publish_version('docs', jsonb_build_object('version_id', :'vid'));")
        writer = Session(db, 'pgev_writer', '\n'.join(script) + '\n')
        # Independent plan branches must agree, not just two rows of one join.
        plan = {'nodes': [
            {'id': 'a', 'op': 'literal', 'text': 'token alpha', 'limit': 1},
            {'id': 'b', 'op': 'literal', 'text': 'token beta', 'limit': 1},
            {'id': 'both', 'op': 'union', 'inputs': ['a', 'b']}], 'output': 'both'}
        observed = 0
        while writer.proc.poll() is None or observed == 0:
            out = db.query('docs', plan)
            versions = {r['version_id'] for r in out['results']}
            self.assertEqual(len(out['results']), 2, out)
            self.assertEqual(len(versions), 1, f'mixed snapshot: {out}')
            observed += 1
        rc, _, err = writer.finish()
        self.assertEqual(rc, 0, err)
        self.assertGreater(observed, 0)

    def test_resolve_during_purge_returns_a_coherent_state(self):
        asset = self.seed()
        old_id = self.db.scalar('SELECT evidence_id FROM docs.spans;')
        old_version = self.db.scalar('SELECT version_id FROM docs.versions;')
        new = self.db.stage('docs', asset, 'a.md', 'version two', [(0, 7)], 'k1', 1)
        self.db.publish('docs', new['version_id'])
        holder = self.hold_lock()
        purger = Session(self.db, 'pgev_purger',
                         f"BEGIN;\n{self.call('purge', {'version_id': old_version, 'reason': 'concurrent purge'})}\n"
                         f"SELECT pg_advisory_lock({LOCK_KEY});\nCOMMIT;\n")
        self.wait_blocked('pgev_purger', 'advisory')
        # Purge has changed every retained field, but has not committed.
        self.assertEqual(self.db.resolve('docs', old_id)['status'], 'historical')
        sql = f"SELECT evidence.resolve('docs', {lit(old_id)}::uuid);\n"
        reader = Session(self.db, 'pgev_resolver', sql * 100)
        self.release(holder)
        rc, _, err = purger.finish()
        self.assertEqual(rc, 0, err)
        rc, out, err = reader.finish()
        self.assertEqual(rc, 0, err)
        responses = [json.loads(line) for line in out.splitlines() if line]
        self.assertEqual(len(responses), 100)
        for response in responses:
            self.assertIn(response['status'], ('historical', 'purged'))
            if response['status'] == 'historical':
                self.assertEqual(response['text'], 'version')
                self.assertTrue(response['verified'])
            else:
                self.assertNotIn('text', response)
                self.assertEqual(response['reason'], 'concurrent purge')
        self.assertEqual(self.db.resolve('docs', old_id)['status'], 'purged')

    def test_statement_timeout_cancels_query(self):
        db = self.db
        db.init('docs', {'limits': {'max_response_bytes': 16 << 20}})
        src = ''.join(f'{i:015d}\n' for i in range(40000))
        spans = [(i * 16, i * 16 + 15) for i in range(40000)]
        db.stage('docs', new_id(), 'big.md', src, spans, 'k', 0)
        v = db.scalar('SELECT version_id FROM docs.versions;')
        db.publish('docs', v)
        plan = {'nodes': [{'id': 'a', 'op': 'regex', 'pattern': '(0|1|2|3|4|5|6|7|8|9)+9$'}], 'output': 'a'}
        start = time.monotonic()
        err = db.api_error('query', 'docs', plan, prefix="SET statement_timeout = '1ms';")
        self.assertIsNotNone(err)
        self.assertEqual(err.sqlstate, '57014')
        self.assertLess(time.monotonic() - start, 30)
        self.assertEqual(db.scalar("SELECT count(*) FROM pg_stat_activity WHERE state = 'active' "
                                   "AND query LIKE '%evidence.query%' AND pid <> pg_backend_pid();"), '0')

    def test_link_retries_when_unlink_wins_between_insert_and_select(self):
        self.db.init('docs')
        staged = self.db.stage('docs', new_id(), 'links.md', 'claim support',
                               [(0, 5), (6, 13)], 'links', 0)
        request = {'action': 'link', 'kind': 'cites',
                   'source_evidence_id': staged['spans'][0]['evidence_id'],
                   'target_evidence_id': staged['spans'][1]['evidence_id']}
        self.db.api('annotate', 'docs', request)
        # A test-only statement trigger pauses even when ON CONFLICT inserts
        # zero rows. This places unlink exactly between INSERT and reselect.
        self.db.run(f"""
          CREATE FUNCTION docs.pause_link() RETURNS trigger LANGUAGE plpgsql AS $$
          BEGIN PERFORM pg_advisory_xact_lock({LOCK_KEY}); RETURN NULL; END $$;
          CREATE TRIGGER pause_link AFTER INSERT ON docs.relations
            FOR EACH STATEMENT EXECUTE FUNCTION docs.pause_link();
        """)
        holder = self.hold_lock()
        linker = Session(self.db, 'pgev_linker', self.call('annotate', request) + '\n')
        self.wait_blocked('pgev_linker', 'advisory')
        removed = self.db.api('annotate', 'docs', dict(request, action='unlink'))
        self.assertEqual(removed['status'], 'unlinked')
        self.release(holder)
        rc, out, err = linker.finish()
        self.assertEqual(rc, 0, err)
        self.assertEqual(json.loads(out.strip())['status'], 'linked')
        self.assertEqual(self.db.scalar('SELECT count(*) FROM docs.relations;'), '1')


if __name__ == '__main__':
    unittest.main()
