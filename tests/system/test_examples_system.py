"""The examples run against a real server: quickstart, importer, agent tool."""
import json
import subprocess
import sys
import tempfile
import unittest
from pathlib import Path

from pgev import REPO, SystemTestCase, env_for, psql_args, stage_request, new_id

sys.path.insert(0, str(REPO / 'examples'))
from import_files import chunk_spans


class ChunkSpansTest(unittest.TestCase):
    def test_preserves_utf8_bytes_and_respects_limits(self):
        for source in ['', 'a' * 10007, 'é😀漢\n' * 1000, '\r\n' * 2000]:
            raw = source.encode('utf-8')
            for limit in (4, 7, 2000):
                with self.subTest(limit=limit, bytes=len(raw)):
                    spans = chunk_spans(raw, limit)
                    parts = [raw[start:end] for start, end in spans]
                    self.assertEqual(b''.join(parts), raw)
                    self.assertTrue(all(0 < len(part) <= limit for part in parts))
                    for part in parts:
                        part.decode('utf-8')

    def test_prefers_nearby_newline_without_wasting_half_a_chunk(self):
        data = (b'x' * 1000 + b'\n') * 1047
        spans = chunk_spans(data, 2000)
        self.assertTrue(all(end - start >= 1800 for start, end in spans[:-1]))
        self.assertEqual(chunk_spans(b'x' * 1899 + b'\n' + b'y' * 200, 2000),
                         [(0, 1900), (1900, 2100)])


class ExamplesTest(SystemTestCase):
    def test_default_importer_accepts_near_limit_line_aligned_file(self):
        with tempfile.TemporaryDirectory() as tmp:
            root = Path(tmp)
            content = (b'x' * 1000 + b'\n') * 1047
            (root / 'large.txt').write_bytes(content)
            proc = self.run_example(str(REPO / 'examples' / 'import_files.py'),
                                    '--corpus', 'docs', '--root', tmp, '--init')
            self.assertEqual(proc.returncode, 0, proc.stderr + proc.stdout)
            result = json.loads(proc.stdout)
            self.assertEqual(result['revision'], 1)
            self.assertEqual(self.db.scalar('SELECT octet_length(source) FROM docs.versions;'), str(len(content)))
            self.assertEqual(self.db.scalar(
                "SELECT string_agg(text, '' ORDER BY start_byte) = (SELECT source FROM docs.versions) FROM docs.spans;"), 't')

    def run_example(self, *args, env_extra=None):
        return subprocess.run([sys.executable, *args], capture_output=True, text=True,
                              env=env_for(self.dbname, **(env_extra or {})), timeout=300)

    def test_client_preserves_quotes_and_backslashes_with_legacy_strings(self):
        self.db.init('docs')
        source = "A backslash \\ and quote ' must remain data."
        request = stage_request(new_id(), 'quotes.txt', source, [(0, len(source))], 'quote-test', 0)
        code = (f"import sys,json; sys.path.insert(0,{str(REPO / 'examples')!r}); "
                "from evidence_client import Client; "
                f"print(json.dumps(Client('docs').call('stage_version',{request!r})))")
        result = self.run_example('-c', code,
                                  env_extra={'PGOPTIONS': '-c standard_conforming_strings=off'})
        self.assertEqual(result.returncode, 0, result.stderr)
        staged = json.loads(result.stdout)
        self.assertEqual(self.db.resolve('docs', staged['spans'][0]['evidence_id'])['text'], source)

    def test_quickstart_script(self):
        proc = subprocess.run(psql_args('-f', str(REPO / 'examples' / 'quickstart.sql')),
                              capture_output=True, text=True, env=env_for(self.dbname), timeout=120)
        self.assertEqual(proc.returncode, 0, proc.stderr)
        self.assertIn('"status": "historical"', proc.stdout)
        self.assertIn('a lost response replays the stored IDs.', proc.stdout)

    def test_importer_and_agent_tool(self):
        with tempfile.TemporaryDirectory() as tmp:
            root = Path(tmp)
            (root / 'docs').mkdir()
            (root / 'docs' / 'retry.md').write_text('# Retries\n\nA lost response replays the stored IDs.\n' * 3)
            (root / 'docs' / 'café.txt').write_text('Unicode path and body: café, 😀\n')
            (root / 'image.bin').write_bytes(b'\x89PNG\x00\x01')
            (root / 'latin1.txt').write_bytes('caf\xe9'.encode('latin-1'))
            deep = root / ('a' * 80) / ('b' * 80) / ('c' * 80)
            deep.mkdir(parents=True)
            (deep / 'deep.md').write_text('A document under a long relative path.')
            with (root / 'too-big.txt').open('wb') as large:
                large.truncate((1 << 20) + 1)
            importer = str(REPO / 'examples' / 'import_files.py')
            first = self.run_example(importer, '--corpus', 'docs', '--root', tmp, '--init')
            self.assertEqual(first.returncode, 0, first.stderr + first.stdout)
            lines = {r['path']: r for r in map(json.loads, first.stdout.splitlines())}
            self.assertEqual(lines['image.bin']['skipped'], 'binary')
            self.assertTrue(lines['latin1.txt']['skipped'].startswith('not UTF-8'))
            self.assertTrue(lines['too-big.txt']['skipped'].startswith('larger than max_source_bytes'))
            self.assertEqual(lines[(deep / 'deep.md').relative_to(root).as_posix()]['revision'], 1)
            self.assertEqual(lines['docs/retry.md']['revision'], 1)

            again = self.run_example(importer, '--corpus', 'docs', '--root', tmp)
            self.assertTrue(json.loads([l for l in again.stdout.splitlines() if 'retry.md' in l][0])['unchanged'])
            (root / 'docs' / 'retry.md').write_text('# Retries\n\nAn identical retry returns the stored IDs.\n')
            edited = self.run_example(importer, '--corpus', 'docs', '--root', tmp)
            self.assertEqual(json.loads([l for l in edited.stdout.splitlines() if 'retry.md' in l][0])['revision'], 2)

        tool = str(REPO / 'examples' / 'agent_tool.py')
        schema = self.run_example(tool, 'schema')
        self.assertEqual([t['name'] for t in json.loads(schema.stdout)], ['evidence_search', 'evidence_cite'])
        found = self.run_example(tool, 'search', 'docs', 'identical retry', '--hops')
        self.assertEqual(found.returncode, 0, found.stderr + found.stdout)
        result = json.loads(found.stdout)
        self.assertEqual(result['results'][0]['path'], 'docs/retry.md')
        cited = self.run_example(tool, 'cite', 'docs', result['results'][0]['evidence_id'])
        self.assertEqual(json.loads(cited.stdout)['status'], 'current')


if __name__ == '__main__':
    unittest.main()
