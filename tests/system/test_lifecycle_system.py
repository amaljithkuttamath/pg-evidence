"""Single-session system tests through psql: lifecycle, Unicode bytes, retry and
conflict rules, budgets, corruption, roles, search_path spoofing, encoding."""
import json
import unittest

from pgev import (Database, SystemTestCase, byte_span, lit, new_id, sha,
                  stage_request)

UNICODE_A = '\ufeffCafé cafe\u0301\r\n😀 «quoted» end\n'
UNICODE_B = '\ufeffCafé cafe\u0301\r\n😀 «edited» end\n'


class LifecycleTest(SystemTestCase):
    def test_history_unicode_retry_and_conflicts(self):
        db = self.db
        db.init('docs')
        asset = new_id()
        spans = [byte_span(UNICODE_A, 'cafe\u0301'), byte_span(UNICODE_A, '😀 «quoted»')]
        a = db.stage('docs', asset, 'notes/café.md', UNICODE_A, spans, 'k-a', 0)
        self.assertFalse(a['replayed'])
        # Lost response: the identical request replays the stored IDs.
        again = db.stage('docs', asset, 'notes/café.md', UNICODE_A, spans, 'k-a', 0)
        self.assertTrue(again['replayed'])
        self.assertEqual(again['spans'], a['spans'])
        conflict = db.api_error('stage_version', 'docs',
                                stage_request(asset, 'notes/café.md', UNICODE_B, [], 'k-a', 0))
        self.assertEqual((conflict.sqlstate, conflict.reason), ('23505', 'ingestion_key_reused'))

        self.assertEqual(db.publish('docs', a['version_id'])['revision'], 1)
        b = db.stage('docs', asset, 'notes/café.md', UNICODE_B, [byte_span(UNICODE_B, '😀 «edited»')], 'k-b', 1)
        stale = db.stage('docs', asset, 'notes/café.md', UNICODE_B + 'x', [], 'k-c', 1)
        self.assertEqual(db.publish('docs', b['version_id'])['revision'], 2)
        err = db.api_error('publish_version', 'docs', {'version_id': stale['version_id']})
        self.assertEqual((err.sqlstate, err.reason, err.detail['current_revision']), ('55000', 'revision_conflict', 2))

        old = db.resolve('docs', a['spans'][1]['evidence_id'])
        self.assertEqual((old['status'], old['text']), ('historical', '😀 «quoted»'))
        self.assertEqual(old['source_sha256'], sha(UNICODE_A))
        self.assertEqual(db.resolve('docs', a['spans'][0]['evidence_id'])['text'], 'cafe\u0301')
        self.assertEqual(db.resolve('docs', b['spans'][0]['evidence_id'])['status'], 'current')
        # Stored bytes are exactly the received bytes.
        stored = db.scalar(f"SELECT encode(convert_to(source, 'UTF8'), 'hex') FROM docs.versions "
                           f"WHERE version_id = {lit(a['version_id'])}::uuid;")
        self.assertEqual(bytes.fromhex(stored), UNICODE_A.encode('utf-8'))

    def test_new_asset_with_stale_revision_writes_nothing(self):
        self.db.init('docs')
        err = self.db.api_error('stage_version', 'docs', stage_request(new_id(), 'a.md', 'x', [], 'k', 3))
        self.assertEqual((err.sqlstate, err.reason), ('55000', 'revision_conflict'))
        self.assertEqual(self.db.scalar('SELECT count(*) FROM docs.assets;'), '0')

    def test_query_budgets_are_exact(self):
        db = self.db
        db.init('docs')
        src = ' '.join(f'needle number {i} é.' for i in range(20))
        spans = []
        pos = 0
        for _ in range(20):
            spans.append(byte_span(src, 'needle number', pos))
            pos = spans[-1][1]
        v = db.stage('docs', new_id(), 'n.md', src, spans, 'k', 0)
        db.publish('docs', v['version_id'])
        plan = {'nodes': [{'id': 'a', 'op': 'literal', 'text': 'needle', 'limit': 20}], 'output': 'a'}
        full = db.scalar(f"SELECT octet_length(evidence.query('docs', {lit(json.dumps(plan))}::jsonb)::text);")
        for budget in (int(full), int(full) - 1, 1500, 700):
            p = dict(plan, max_response_bytes=budget)
            size = int(db.scalar(f"SELECT octet_length(evidence.query('docs', {lit(json.dumps(p))}::jsonb)::text);"))
            self.assertLessEqual(size, budget)
            out = db.query('docs', p)
            kept = len(out['results'])
            self.assertEqual(out['truncation']['dropped_for_budget'], 20 - kept)
            self.assertEqual(out['complete'], kept == 20)
        tiny = db.api_error('query', 'docs', dict(plan, max_response_bytes=100))
        self.assertEqual((tiny.sqlstate, tiny.reason), ('54000', 'response_envelope_too_large'))
        unset = db.api_error('query', 'docs', plan, prefix='SET statement_timeout = 0;')
        self.assertEqual((unset.sqlstate, unset.reason), ('55000', 'statement_timeout_unset'))

    def test_corruption_is_detected(self):
        db = self.db
        db.init('docs')
        v = db.stage('docs', new_id(), 'a.md', 'trusted bytes', [(0, 7), (8, 13)], 'k', 0)
        db.publish('docs', v['version_id'])
        db.run(f"UPDATE docs.spans SET text = 'TRUSTED' WHERE evidence_id = {lit(v['spans'][0]['evidence_id'])}::uuid;")
        err = db.error(f"SELECT evidence.resolve('docs', {lit(v['spans'][0]['evidence_id'])}::uuid);")
        self.assertEqual((err.sqlstate, err.reason), ('XX001', 'span_mismatch'))
        self.assertEqual(db.error("UPDATE docs.versions SET source = 'trusted BYTES';").sqlstate, '23514')
        db.run("ALTER TABLE docs.versions DROP CONSTRAINT versions_source_digest;"
               "UPDATE docs.versions SET source = 'trusted BYTES';")
        err = db.error(f"SELECT evidence.resolve('docs', {lit(v['spans'][1]['evidence_id'])}::uuid);")
        self.assertEqual((err.sqlstate, err.reason), ('XX001', 'digest_mismatch'))

    def test_purge_retire_and_status(self):
        db = self.db
        db.init('docs')
        asset = new_id()
        a = db.stage('docs', asset, 'a.md', 'secret words', [(0, 6)], 'k1', 0)
        db.publish('docs', a['version_id'])
        err = db.api_error('purge', 'docs', {'version_id': a['version_id'], 'reason': 'x'})
        self.assertEqual((err.sqlstate, err.reason), ('55000', 'version_current'))
        b = db.stage('docs', asset, 'a.md', 'public words', [(0, 6)], 'k2', 1)
        db.publish('docs', b['version_id'])
        p = db.api('purge', 'docs', {'version_id': a['version_id'], 'reason': 'request 7'})
        self.assertEqual((p['status'], p['spans_purged']), ('purged', 1))
        r = db.resolve('docs', a['spans'][0]['evidence_id'])
        self.assertEqual((r['status'], r['reason']), ('purged', 'request 7'))
        self.assertNotIn('text', r)
        retired = db.api('retire', 'docs', {'asset_id': asset, 'expected_revision': 2})
        self.assertEqual(retired['content_revision'], 3)
        self.assertEqual(db.resolve('docs', b['spans'][0]['evidence_id'])['status'], 'retired')

    def test_roles_deny_direct_byte_edits(self):
        db = self.db
        db.init('docs')
        roles = self.create_roles('pgevlc')
        self.grant('docs', roles)
        w, p, r = roles['writer'], roles['purger'], roles['reader']
        asset = new_id()
        a = db.api('stage_version', 'docs', stage_request(asset, 'a.md', 'kept bytes', [(0, 4)], 'k1', 0),
                   prefix=f'SET ROLE {w};')
        db.api('publish_version', 'docs', {'version_id': a['version_id']}, prefix=f'SET ROLE {w};')
        b = db.api('stage_version', 'docs', stage_request(asset, 'a.md', 'new bytes', [(0, 3)], 'k2', 1),
                   prefix=f'SET ROLE {w};')
        db.api('publish_version', 'docs', {'version_id': b['version_id']}, prefix=f'SET ROLE {w};')
        for sql in ("UPDATE docs.versions SET source = 'x';", "UPDATE docs.versions SET path = 'x';",
                    'DELETE FROM docs.versions;', "UPDATE docs.spans SET text = 'x';", 'DELETE FROM docs.spans;',
                    'UPDATE docs.publications SET revision = 9;', 'DELETE FROM docs.publications;',
                    'TRUNCATE docs.spans;', 'UPDATE docs.collection_config SET max_source_bytes = 1;'):
            err = db.error(f'SET ROLE {w};' + sql)
            self.assertIsNotNone(err, sql)
            self.assertEqual(err.sqlstate, '42501', sql)
        self.assertIsNone(db.error(f'SET ROLE {w}; BEGIN; SELECT * FROM docs.assets FOR UPDATE; ROLLBACK;'))
        err = db.api_error('purge', 'docs', {'version_id': a['version_id'], 'reason': 'x'}, prefix=f'SET ROLE {w};')
        self.assertEqual(err.sqlstate, '42501')
        done = db.api('purge', 'docs', {'version_id': a['version_id'], 'reason': 'ok'}, prefix=f'SET ROLE {p};')
        self.assertEqual(done['status'], 'purged')
        plan = {'nodes': [{'id': 'a', 'op': 'literal', 'text': 'new'}], 'output': 'a'}
        self.assertEqual(len(db.api('query', 'docs', plan, prefix=f'SET ROLE {r};')['results']), 1)
        err = db.api_error('stage_version', 'docs', stage_request(new_id(), 'z.md', 'z', [], 'k3', 0), prefix=f'SET ROLE {r};')
        self.assertEqual(err.sqlstate, '42501')

    def test_hostile_search_path_does_not_change_results(self):
        db = self.db
        db.init('docs')
        v = db.stage('docs', new_id(), 'a.md', 'harmless text', [(0, 8)], 'k', 0)
        db.publish('docs', v['version_id'])
        plan = {'nodes': [{'id': 'a', 'op': 'literal', 'text': 'absent'}], 'output': 'a'}
        db.run("CREATE SCHEMA evil;"
               "CREATE FUNCTION evil.strpos(text, text) RETURNS integer LANGUAGE sql AS 'SELECT 1';"
               "CREATE FUNCTION evil.uuid_eq(uuid, uuid) RETURNS boolean LANGUAGE sql AS 'SELECT true';"
               "CREATE OPERATOR evil.= (LEFTARG = uuid, RIGHTARG = uuid, FUNCTION = evil.uuid_eq);"
               "CREATE TABLE evil.spans (evidence_id uuid);")
        out = db.api('query', 'docs', plan, prefix='SET search_path = evil, public, pg_catalog;')
        self.assertEqual(out['results'], [])
        hit = db.api('query', 'docs', dict(plan, nodes=[{'id': 'a', 'op': 'literal', 'text': 'harmless'}]),
                     prefix='SET search_path = evil, pg_catalog;')
        self.assertEqual(len(hit['results']), 1)
        r = db.json(f"SET search_path = evil, pg_catalog; SELECT evidence.resolve('docs', {lit(v['spans'][0]['evidence_id'])}::uuid);")
        self.assertEqual(r['text'], 'harmless')

    def test_input_rejected_by_postgres_keeps_its_sqlstate(self):
        self.db.init('docs')
        self.assertEqual(self.db.error("SELECT evidence.stage_version('docs', '{bad'::jsonb);").sqlstate, '22P02')
        self.assertEqual(self.db.error("SELECT evidence.stage_version('docs', '{\"s\": \"\\u0000\"}'::jsonb);").sqlstate, '22P05')
        deep = '{"x": ' + '[' * 300 + ']' * 300 + '}'
        self.assertEqual(self.db.error(f"SELECT evidence.stage_version('docs', {lit(deep)}::jsonb);").sqlstate, '22023')


class EncodingTest(SystemTestCase):
    extensions = ()

    def test_non_utf8_database_is_refused(self):
        name = self.dbname + '_ascii'
        self.admin.run(f"CREATE DATABASE {name} TEMPLATE template0 ENCODING 'SQL_ASCII' LC_COLLATE 'C' LC_CTYPE 'C';")
        self.addCleanup(self.drop_database, name)
        db = Database(name)
        db.run('CREATE EXTENSION pg_evidence;')
        err = db.error("SELECT evidence.init_collection('docs', '{}'::jsonb);")
        self.assertEqual((err.sqlstate, err.reason), ('55000', 'non_utf8_database'))
        latin = self.dbname + '_latin'
        self.admin.run(f"CREATE DATABASE {latin} TEMPLATE template0 ENCODING 'LATIN1' LC_COLLATE 'C' LC_CTYPE 'C';")
        self.addCleanup(self.drop_database, latin)
        latin_db = Database(latin)
        latin_db.run('CREATE EXTENSION pg_evidence;')
        err = latin_db.error("SELECT evidence.init_collection('docs', '{\"unknown\": \"café\"}'::jsonb);")
        self.assertEqual((err.sqlstate, err.reason), ('55000', 'non_utf8_database'))



if __name__ == '__main__':
    unittest.main()
