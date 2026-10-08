"""Matched SQL baseline: identical catalog, and a differential check of the
extension against independent SQL over separately loaded copies of one corpus.
The baseline arm never calls pg_evidence functions."""
import json
import unittest

from pgev import REPO, SystemTestCase, byte_span, lit, new_id

CATALOG_SQL = """
SELECT json_agg(x ORDER BY x) FROM (
  SELECT 'col ' || c.relname || '.' || a.attname || ' ' || format_type(a.atttypid, a.atttypmod) || ' '
         || a.attnotnull::text || ' ' || a.attgenerated::text || ' ' || coalesce(pg_get_expr(d.adbin, d.adrelid), '') AS x
  FROM pg_class c JOIN pg_attribute a ON a.attrelid = c.oid AND a.attnum > 0 AND NOT a.attisdropped
  LEFT JOIN pg_attrdef d ON d.adrelid = c.oid AND d.adnum = a.attnum
  WHERE c.relnamespace = {s}::regnamespace AND c.relkind = 'r'
  UNION ALL
  SELECT 'con ' || c.relname || ' ' || k.conname || ' ' || pg_get_constraintdef(k.oid)
  FROM pg_constraint k JOIN pg_class c ON c.oid = k.conrelid WHERE k.connamespace = {s}::regnamespace
  UNION ALL
  SELECT 'idx ' || i.relname || ' ' || pg_get_indexdef(i.oid)
  FROM pg_index x JOIN pg_class i ON i.oid = x.indexrelid JOIN pg_class t ON t.oid = x.indrelid
  WHERE t.relnamespace = {s}::regnamespace) z;
"""

CONFIG_SQL = """SELECT row_to_json(c)::jsonb - 'created_at' FROM {s}.collection_config c;"""

DOCS = [
    ('guide/retry.md', 'Retry semantics: a lost response is replayed. Retry keys are unique.',
     ['Retry semantics', 'a lost response is replayed', 'Retry keys are unique']),
    ('guide/purge.md', 'Purge removes bytes but keeps identifiers and a tombstone.',
     ['Purge removes bytes', 'keeps identifiers and a tombstone']),
    ('notes/é.md', 'Notes: the replayed response matches; résumé of retry behaviour.',
     ['Notes', 'the replayed response matches', 'résumé of retry behaviour']),
]
LINKS = [(('notes/é.md', 'the replayed response matches'), ('guide/retry.md', 'a lost response is replayed'), 'cites'),
         (('guide/purge.md', 'Purge removes bytes'), ('guide/retry.md', 'Retry keys are unique'), 'related'),
         (('guide/retry.md', 'a lost response is replayed'), ('guide/purge.md', 'Purge removes bytes'), 'explains')]


def normalize(results):
    return [(r['path'], r['start_byte'], r['end_byte'], r['status'], r['mode'], r['excerpt'], r['excerpt_truncated'])
            for r in results]


class BaselineTest(SystemTestCase):
    extensions = ('vector', 'pg_evidence')

    def catalog(self, schema):
        text = json.dumps(self.db.json(CATALOG_SQL.format(s=lit(schema))))
        return json.loads(text.replace(f'{schema}.', 'S.'))

    def test_baseline_schema_matches_init_collection(self):
        self.db.init('ext_c', {'text_search_config': 'english'})
        self.db.run_file(REPO / 'baseline' / 'schema.sql', {'corpus': 'sql_c', 'ts_config': 'pg_catalog.english'})
        self.assertEqual(self.catalog('ext_c'), self.catalog('sql_c'))
        self.assertEqual(self.db.json(CONFIG_SQL.format(s='ext_c')), self.db.json(CONFIG_SQL.format(s='sql_c')))

        self.db.init('ext_v', {'embedding_model': 'm', 'embedding_dimensions': 3})
        vs = self.db.scalar("SELECT extnamespace::regnamespace::text FROM pg_extension WHERE extname = 'vector';")
        self.db.run_file(REPO / 'baseline' / 'schema.sql',
                         {'corpus': 'sql_v', 'vector_schema': vs, 'dims': 3, 'embedding_model': 'm'})
        self.assertEqual(self.catalog('ext_v'), self.catalog('sql_v'))
        self.assertEqual(self.db.json(CONFIG_SQL.format(s='ext_v')), self.db.json(CONFIG_SQL.format(s='sql_v')))

    # Differential retrieval --------------------------------------------------
    def baseline(self, corpus, sql):
        out = self.db.run_file(REPO / 'baseline' / 'wrapper.sql', {'corpus': corpus}, sql)
        return json.loads(out.rstrip('\n').split('\n')[-1])

    def load_both(self):
        db = self.db
        db.init('ext_c')
        db.run_file(REPO / 'baseline' / 'schema.sql', {'corpus': 'sql_c'})
        for i, (path, source, fragments) in enumerate(DOCS):
            spans = [byte_span(source, f) for f in fragments]
            v = db.stage('ext_c', new_id(), path, source, spans, f'k{i}', 0)
            db.publish('ext_c', v['version_id'])
            starts = '{' + ','.join(str(s) for s, _ in spans) + '}'
            ends = '{' + ','.join(str(e) for _, e in spans) + '}'
            staged = self.baseline('sql_c', f"EXECUTE baseline_stage({lit(new_id())}, {lit(path)}, {lit(source)}, "
                                            f"{lit(f'k{i}')}, 0, {lit(starts)}, {lit(ends)});")
            published = self.baseline('sql_c', f"EXECUTE baseline_publish({lit(staged['version_id'])});")
            self.assertEqual(published['revision'], 1)
        for (src_doc, src_frag), (dst_doc, dst_frag), kind in LINKS:
            ids = {}
            for corpus in ('ext_c', 'sql_c'):
                def eid(path, frag):
                    source = next(d[1] for d in DOCS if d[0] == path)
                    s, e = byte_span(source, frag)
                    return db.scalar(f"SELECT s.evidence_id FROM {corpus}.spans s JOIN {corpus}.versions v USING (version_id) "
                                     f"WHERE v.path = {lit(path)} AND s.start_byte = {s} AND s.end_byte = {e};")
                ids[corpus] = (eid(src_doc, src_frag), eid(dst_doc, dst_frag))
            db.api('annotate', 'ext_c', {'action': 'link', 'source_evidence_id': ids['ext_c'][0],
                                         'target_evidence_id': ids['ext_c'][1], 'kind': kind})
            self.baseline('sql_c', f"EXECUTE baseline_link({lit(ids['sql_c'][0])}, {lit(kind)}, {lit(ids['sql_c'][1])}); "
                                   f"SELECT '{{}}';")

    def test_extension_matches_independent_sql(self):
        self.load_both()
        db = self.db
        for needle in ('Retry', 'replayed', 'é', 'absent'):
            ext = db.query('ext_c', {'nodes': [{'id': 'a', 'op': 'literal', 'text': needle, 'limit': 5}],
                                     'output': 'a', 'excerpt_bytes': 12})
            base = self.baseline('sql_c', f"EXECUTE baseline_literal({lit(needle)}, 5, 12, 65536);")
            self.assertEqual(normalize(ext['results']), normalize(base['results']), needle)
            for k in ('requested', 'returned', 'truncated', 'underfilled', 'dropped_for_budget'):
                self.assertEqual(ext['truncation'][k], base['truncation'][k], (needle, k))

        for q in ('replayed response', 'retry', 'tombstone'):
            ext = db.query('ext_c', {'nodes': [{'id': 'a', 'op': 'lexical', 'query': q, 'limit': 4}], 'output': 'a'})
            base = self.baseline('sql_c', f"EXECUTE baseline_lexical({lit(q)}, 4, 512, 65536);")
            self.assertEqual(sorted(normalize(ext['results'])), sorted(normalize(base['results'])), q)
            self.assertEqual(sorted(round(r['rank'], 6) for r in ext['results']),
                             sorted(round(r['rank'], 6) for r in base['results']), q)

        plan = {'nodes': [{'id': 'hits', 'op': 'lexical', 'query': 'replayed', 'limit': 3},
                          {'id': 'near', 'op': 'neighbors', 'from': 'hits', 'limit': 5, 'max_edges': 20},
                          {'id': 'all', 'op': 'union', 'inputs': ['hits', 'near']}], 'output': 'all'}
        ext = db.query('ext_c', plan)
        base = self.baseline('sql_c', "EXECUTE baseline_lexical_neighbors('replayed', 3, 512, 65536, 5, 20);")
        self.assertEqual(sorted(normalize(ext['results'])), sorted(normalize(base['results'])))
        self.assertGreater(len(ext['results']), 2)

        for r in ext['results']:
            ext_r = db.resolve('ext_c', r['evidence_id'])
            sid = db.scalar(f"SELECT s.evidence_id FROM sql_c.spans s JOIN sql_c.versions v USING (version_id) "
                            f"WHERE v.path = {lit(r['path'])} AND s.start_byte = {r['start_byte']} AND s.end_byte = {r['end_byte']};")
            base_r = self.baseline('sql_c', f"EXECUTE baseline_resolve({lit(sid)});")
            self.assertEqual((ext_r['status'], ext_r['text']), (base_r['status'], base_r['text']))
            self.assertTrue(ext_r['verified'] and base_r['verified'])


if __name__ == '__main__':
    unittest.main()
