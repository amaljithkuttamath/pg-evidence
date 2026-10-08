"""G8: extension drop/recreate and dump/restore keep evidence bytes, IDs,
digests, statuses and grants."""
import shutil
import subprocess
import tempfile
import unittest
import uuid
from pathlib import Path

from pgev import Database, SystemTestCase, byte_span, env_for, lit, new_id

SOURCE_A = 'Résumé line one\r\nsecond 😀 line\n'
SOURCE_B = 'Résumé line one\r\nsecond 🙂 line, edited\n'

SNAPSHOT_SQL = """
SELECT json_build_object(
  'spans', (SELECT json_agg(json_build_array(evidence_id, version_id, start_byte, end_byte, text) ORDER BY evidence_id) FROM docs.spans),
  'versions', (SELECT json_agg(json_build_array(version_id, asset_id, path, encode(source_sha256, 'hex'),
               encode(convert_to(source, 'UTF8'), 'hex'), byte_length, ingestion_key) ORDER BY version_id) FROM docs.versions),
  'assets', (SELECT json_agg(json_build_array(asset_id, current_version_id, content_revision, annotation_revision) ORDER BY asset_id) FROM docs.assets),
  'publications', (SELECT json_agg(json_build_array(version_id, revision) ORDER BY version_id) FROM docs.publications),
  'tombstones', (SELECT json_agg(json_build_array(version_id, reason) ORDER BY version_id) FROM docs.tombstones),
  'relations', (SELECT json_agg(json_build_array(source_evidence_id, kind, target_evidence_id) ORDER BY 1, 2, 3) FROM docs.relations),
  'tags', (SELECT json_agg(json_build_array(asset_id, tag) ORDER BY 1, 2) FROM docs.tags),
  'embeddings', (SELECT json_agg(json_build_array(evidence_id, embedding::text) ORDER BY evidence_id) FROM docs.embeddings),
  'acl', (SELECT json_agg(json_build_array(relname, relacl::text) ORDER BY relname) FROM pg_class
          WHERE relnamespace = 'docs'::regnamespace AND relkind = 'r'));
"""


class RestoreTest(SystemTestCase):
    extensions = ('vector', 'pg_evidence')

    def build_corpus(self):
        db = self.db
        db.init('docs', {'embedding_model': 'test-2d', 'embedding_dimensions': 2})
        roles = self.create_roles('pgevrs')
        self.grant('docs', roles)
        asset = new_id()
        a = db.stage('docs', asset, 'r.md', SOURCE_A,
                     [byte_span(SOURCE_A, 'Résumé line one'), byte_span(SOURCE_A, 'second 😀 line')], 'k1', 0)
        db.run(f"SELECT evidence.attach_embeddings('docs', {lit(_attach(a, [[1, 0], [0, 1]]))}::jsonb);")
        db.publish('docs', a['version_id'])
        b = db.stage('docs', asset, 'r.md', SOURCE_B, [byte_span(SOURCE_B, 'Résumé line one')], 'k2', 1)
        db.run(f"SELECT evidence.attach_embeddings('docs', {lit(_attach(b, [[1, 1]]))}::jsonb);")
        db.publish('docs', b['version_id'])
        db.api('annotate', 'docs', {'action': 'tag', 'asset_id': asset, 'tags': ['kept'], 'expected_annotation_revision': 0})
        db.api('annotate', 'docs', {'action': 'link', 'source_evidence_id': b['spans'][0]['evidence_id'],
                                    'target_evidence_id': a['spans'][1]['evidence_id'], 'kind': 'replaces'})
        db.api('purge', 'docs', {'version_id': a['version_id'], 'reason': 'restore test'})
        return roles, a, b

    def check_resolution(self, db, a, b):
        current = db.resolve('docs', b['spans'][0]['evidence_id'])
        self.assertEqual((current['status'], current['text'], current['verified']), ('current', 'Résumé line one', True))
        purged = db.resolve('docs', a['spans'][1]['evidence_id'])
        self.assertEqual((purged['status'], purged['reason']), ('purged', 'restore test'))
        out = db.query('docs', {'nodes': [{'id': 's', 'op': 'semantic', 'model': 'test-2d', 'vector': [1, 1]}], 'output': 's'})
        self.assertEqual(out['results'][0]['evidence_id'], b['spans'][0]['evidence_id'])

    def test_drop_extension_keeps_corpus_and_recreate_resumes(self):
        roles, a, b = self.build_corpus()
        before = self.db.json(SNAPSHOT_SQL)
        self.db.run('DROP EXTENSION pg_evidence;')  # no CASCADE: nothing depends on it
        self.assertEqual(self.db.json(SNAPSHOT_SQL), before)
        # The functions are gone (the auto-created schema may or may not remain).
        gone = self.db.error("SELECT evidence.resolve('docs', gen_random_uuid());")
        self.assertIn(gone.sqlstate, ('3F000', '42883'))
        self.db.run('CREATE EXTENSION pg_evidence;')
        self.db.run(f'GRANT USAGE ON SCHEMA evidence TO {roles["reader"]};')  # in case the schema was recreated
        self.check_resolution(self.db, a, b)
        self.db.run('DROP EXTENSION pg_evidence CASCADE;')
        self.assertEqual(self.db.json(SNAPSHOT_SQL), before)
        self.db.run('CREATE EXTENSION pg_evidence;')
        self.check_resolution(self.db, a, b)

    def test_dump_and_restore_preserve_bytes_ids_and_grants(self):
        pg_dump, pg_restore = shutil.which('pg_dump'), shutil.which('pg_restore')
        if not (pg_dump and pg_restore):
            self.skipTest('pg_dump/pg_restore not on PATH')
        roles, a, b = self.build_corpus()
        before = self.db.json(SNAPSHOT_SQL)
        with tempfile.TemporaryDirectory() as tmp:
            dump = Path(tmp) / 'corpus.dump'
            subprocess.run([pg_dump, '-Fc', '-f', str(dump)], env=env_for(self.dbname), check=True,
                           capture_output=True, timeout=300)
            target = f'pgev_restore_{uuid.uuid4().hex[:10]}'
            self.admin.run(f'CREATE DATABASE {target} TEMPLATE template0 ENCODING UTF8;')
            self.addCleanup(self.drop_database, target)
            proc = subprocess.run([pg_restore, '--exit-on-error', '--dbname', target, str(dump)],
                                  env=env_for(target), capture_output=True, text=True, timeout=300)
            self.assertEqual(proc.returncode, 0, proc.stderr)
        restored = Database(target)
        self.assertEqual(restored.json(SNAPSHOT_SQL), before)
        self.check_resolution(restored, a, b)
        reader_can = restored.scalar(f"SELECT has_table_privilege({lit(roles['reader'])}, 'docs.spans', 'SELECT')::text "
                                     f"|| has_column_privilege({lit(roles['writer'])}, 'docs.versions', 'source', 'UPDATE')::text;")
        self.assertEqual(reader_can, 'truefalse')


def _attach(staged, vectors):
    import json
    return json.dumps({'version_id': staged['version_id'], 'model': 'test-2d',
                       'embeddings': [{'evidence_id': s['evidence_id'], 'vector': v}
                                      for s, v in zip(staged['spans'], vectors)]})


if __name__ == '__main__':
    unittest.main()
