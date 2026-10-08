"""Helpers for pg-evidence system tests: psql/pg_dump/pg_restore via subprocess.

Connection settings are explicit: the standard PGHOST, PGPORT and PGUSER
environment variables must name a PostgreSQL 18 server where pg_evidence (and,
for embedding tests, pgvector) is installed and PGUSER may create databases and
roles. Tests skip when PGHOST is unset. Each test case creates and drops its own
database; roles it creates are dropped afterwards.
"""
import hashlib
import json
import os
import re
import shutil
import subprocess
import unittest
import uuid
from pathlib import Path

REPO = Path(__file__).resolve().parents[2]
PSQL = shutil.which('psql')
CONFIGURED = bool(os.environ.get('PGHOST')) and PSQL is not None
SKIP_REASON = 'set PGHOST/PGPORT/PGUSER for a PostgreSQL 18 server with pg_evidence installed'
TIMEOUT = 120


def sha(text):
    return hashlib.sha256(text.encode('utf-8')).hexdigest()


def lit(value):
    """SQL string literal (standard_conforming_strings is on by default)."""
    if '\x00' in value:
        raise ValueError('text cannot contain NUL')
    return "'" + value.replace("'", "''") + "'"


def new_id():
    return str(uuid.uuid4())


class SqlError(Exception):
    def __init__(self, sqlstate, message, detail, stderr):
        super().__init__(f'{sqlstate}: {message}')
        self.sqlstate = sqlstate
        self.message = message
        self.detail_text = detail
        try:
            self.detail = json.loads(detail) if detail else {}
        except ValueError:
            self.detail = {}
        self.stderr = stderr

    @property
    def reason(self):
        return self.detail.get('reason')


def env_for(database, **extra):
    env = dict(os.environ)
    env['PGDATABASE'] = database
    env['PGCLIENTENCODING'] = 'UTF8'
    env.setdefault('PGOPTIONS', '-c statement_timeout=30000')
    env.update(extra)
    return env


def psql_args(*more):
    return [PSQL, '-X', '-q', '-A', '-t', '-v', 'ON_ERROR_STOP=1', '-v', 'VERBOSITY=verbose', *more]


def parse_error(stderr):
    m = re.search(r'ERROR:\s+([0-9A-Z]{5}):\s*(.*)', stderr)
    if not m:
        return None
    detail = re.search(r'^DETAIL:\s+(.*)$', stderr, re.M)
    return SqlError(m.group(1), m.group(2), detail.group(1) if detail else '', stderr)


class Database:
    def __init__(self, name):
        self.name = name

    def run(self, sql, variables=None, env_extra=None):
        """Runs a psql script from stdin; returns stdout. Raises SqlError."""
        args = psql_args()
        for k, v in (variables or {}).items():
            args += ['-v', f'{k}={v}']
        proc = subprocess.run(args, input=sql, text=True, capture_output=True,
                              env=env_for(self.name, **(env_extra or {})), timeout=TIMEOUT)
        if proc.returncode != 0:
            err = parse_error(proc.stderr)
            if err:
                raise err
            raise RuntimeError(f'psql failed ({proc.returncode}): {proc.stderr}')
        return proc.stdout

    def run_file(self, path, variables=None, extra_sql=''):
        text = Path(path).read_text()
        return self.run(text + '\n' + extra_sql, variables)

    def scalar(self, sql, **kw):
        out = self.run(sql, **kw).rstrip('\n')
        return out.split('\n')[-1] if out else None

    def json(self, sql, **kw):
        value = self.scalar(sql, **kw)
        return None if value is None else json.loads(value)

    def error(self, sql, **kw):
        try:
            self.run(sql, **kw)
        except SqlError as e:
            return e
        return None

    # API helpers ---------------------------------------------------------
    def api_sql(self, func, corpus, request):
        return f'SELECT evidence.{func}({lit(corpus)}, {lit(json.dumps(request))}::jsonb);'

    def api(self, func, corpus, request, prefix=''):
        return self.json(prefix + self.api_sql(func, corpus, request))

    def api_error(self, func, corpus, request, prefix=''):
        return self.error(prefix + self.api_sql(func, corpus, request))

    def init(self, corpus, config=None):
        self.run(f'SELECT evidence.init_collection({lit(corpus)}, {lit(json.dumps(config or {}))}::jsonb);')

    def stage(self, corpus, asset, path, source, spans, key, expected):
        return self.api('stage_version', corpus, stage_request(asset, path, source, spans, key, expected))

    def publish(self, corpus, version_id):
        return self.api('publish_version', corpus, {'version_id': version_id})

    def resolve(self, corpus, evidence_id):
        return self.json(f'SELECT evidence.resolve({lit(corpus)}, {lit(evidence_id)}::uuid);')

    def query(self, corpus, plan):
        return self.api('query', corpus, plan)


def stage_request(asset, path, source, spans, key, expected):
    return {
        'asset_id': asset, 'path': path, 'source': source, 'source_sha256': sha(source),
        'spans': [{'start_byte': s, 'end_byte': e} for s, e in spans],
        'ingestion_key': key, 'expected_revision': expected,
    }


def byte_span(source, fragment, start_at=0):
    """(start_byte, end_byte) of `fragment` in `source`, in UTF-8 bytes."""
    raw = source.encode('utf-8')
    start = raw.index(fragment.encode('utf-8'), start_at)
    return start, start + len(fragment.encode('utf-8'))


class SystemTestCase(unittest.TestCase):
    """Creates a fresh UTF8 database with pg_evidence per test."""
    extensions = ('pg_evidence',)

    @classmethod
    def setUpClass(cls):
        if not CONFIGURED:
            raise unittest.SkipTest(SKIP_REASON)
        cls.admin = Database(os.environ.get('PGEV_ADMIN_DB', 'postgres'))

    def setUp(self):
        self.dbname = f'pgev_sys_{uuid.uuid4().hex[:12]}'
        self.admin.run(f'CREATE DATABASE {self.dbname} TEMPLATE template0 ENCODING UTF8;')
        self.addCleanup(self.drop_database, self.dbname)
        self.db = Database(self.dbname)
        for ext in self.extensions:
            self.db.run(f'CREATE EXTENSION IF NOT EXISTS {ext};')

    def drop_database(self, name):
        self.admin.run(f'DROP DATABASE IF EXISTS {name} WITH (FORCE);')

    def create_roles(self, prefix):
        roles = {k: f'{prefix}_{k}_{uuid.uuid4().hex[:6]}' for k in ('reader', 'writer', 'purger')}
        names = ', '.join(roles.values())

        def drop_roles():
            # Privileges in this database block DROP ROLE; remove them first.
            self.db.run(f'DROP OWNED BY {names};')
            self.admin.run(f'DROP ROLE IF EXISTS {names};')

        self.addCleanup(drop_roles)
        self.db.run_file(REPO / 'sql' / 'roles.sql', roles)
        return roles

    def grant(self, corpus, roles, database=None):
        (database or self.db).run_file(REPO / 'sql' / 'grants.sql', {'corpus': corpus, **roles})
