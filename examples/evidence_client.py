"""Minimal pg-evidence client over psql (Python stdlib only).

Connection settings come from the standard PG* environment variables
(PGHOST, PGPORT, PGUSER, PGDATABASE, PGPASSWORD or ~/.pgpass). Every call is
one psql process and one SQL statement, so each API call is its own
transaction. SQLSTATE and the JSON error detail are surfaced as EvidenceError.
"""
import json
import os
import re
import shutil
import subprocess

PSQL = os.environ.get('PSQL', shutil.which('psql') or 'psql')


class EvidenceError(Exception):
    def __init__(self, sqlstate, message, detail):
        super().__init__(f'{sqlstate}: {message}')
        self.sqlstate = sqlstate
        self.detail = detail

    @property
    def reason(self):
        return self.detail.get('reason') if isinstance(self.detail, dict) else None


def _literal(text):
    if '\x00' in text:
        raise ValueError('PostgreSQL text cannot contain NUL')
    # Explicit escape strings work with either standard_conforming_strings setting.
    return "E'" + text.replace("\\", "\\\\").replace("'", "''") + "'"


class Client:
    def __init__(self, corpus, statement_timeout='10s'):
        self.corpus = corpus
        self.statement_timeout = statement_timeout

    def sql(self, statement):
        """Runs one statement; returns the first column of the last row."""
        script = f"SET statement_timeout = {_literal(self.statement_timeout)};\n{statement}\n"
        env = dict(os.environ, PGCLIENTENCODING='UTF8')
        proc = subprocess.run([PSQL, '-X', '-q', '-A', '-t', '-v', 'ON_ERROR_STOP=1', '-v', 'VERBOSITY=verbose'],
                              input=script, text=True, capture_output=True, env=env)
        if proc.returncode != 0:
            m = re.search(r'ERROR:\s+([0-9A-Z]{5}):\s*(.*)', proc.stderr)
            d = re.search(r'^DETAIL:\s+(.*)$', proc.stderr, re.M)
            detail = {}
            if d:
                try:
                    detail = json.loads(d.group(1))
                except ValueError:
                    detail = {'text': d.group(1)}
            if m:
                raise EvidenceError(m.group(1), m.group(2), detail)
            raise RuntimeError(proc.stderr.strip())
        lines = proc.stdout.rstrip('\n').split('\n')
        return lines[-1] if lines and lines[-1] else None

    def call(self, function, request):
        if function not in {'init_collection', 'stage_version', 'attach_embeddings',
                            'publish_version', 'retire', 'annotate', 'purge', 'query'}:
            raise ValueError('unknown pg-evidence request function')
        out = self.sql(f'SELECT evidence.{function}({_literal(self.corpus)}, {_literal(json.dumps(request))}::jsonb);')
        return json.loads(out) if out else None

    def resolve(self, evidence_id):
        return json.loads(self.sql(f'SELECT evidence.resolve({_literal(self.corpus)}, {_literal(evidence_id)}::uuid);'))

    def query(self, plan):
        return self.call('query', plan)
