"""Import a directory of UTF-8 text files into a pg-evidence corpus.

    PGDATABASE=mydb python3 examples/import_files.py --corpus docs --root ./repo [--init]

Each file becomes an asset (UUIDv5 of corpus and relative path). Unchanged files
are skipped; changed files are staged against the asset's current revision and
published. Spans are line-aligned chunks of at most --max-span-bytes UTF-8
bytes, cut on character boundaries. Binary (NUL-containing) and non-UTF-8 files
are skipped and reported with their path and reason, as v0.1 stores text only.
One JSON line is printed per file. Embeddings are not computed here.
"""
import argparse
import hashlib
import json
import sys
import uuid
from pathlib import Path

sys.path.insert(0, str(Path(__file__).resolve().parent))
from evidence_client import Client, EvidenceError, _literal  # noqa: E402

NAMESPACE = uuid.UUID('7f0f3c6e-2d4b-5a1e-9c3d-6b8a1e2f4c50')


def chunk_spans(data, max_bytes):
    """Line-aligned (start, end) byte spans of at most max_bytes each."""
    spans, start, pos = [], 0, 0
    for line in data.splitlines(keepends=True):
        if pos + len(line) - start > max_bytes and pos > start:
            spans.append((start, pos))
            start = pos
        pos += len(line)
        while pos - start > max_bytes:  # one long line: cut on a UTF-8 boundary
            cut = start + max_bytes
            while data[cut] & 0xC0 == 0x80:
                cut -= 1
            spans.append((start, cut))
            start = cut
    if pos > start:
        spans.append((start, pos))
    return spans


def import_file(client, root, path, max_span_bytes, max_source_bytes):
    rel = path.relative_to(root).as_posix()
    # Bound reads even if a file grows between stat and open.
    if path.stat().st_size > max_source_bytes:
        return {'path': rel, 'skipped': f'larger than max_source_bytes ({max_source_bytes})'}
    with path.open('rb') as stream:
        data = stream.read(max_source_bytes + 1)
    if len(data) > max_source_bytes:
        return {'path': rel, 'skipped': f'larger than max_source_bytes ({max_source_bytes})'}
    if b'\x00' in data:
        return {'path': rel, 'skipped': 'binary'}
    try:
        text = data.decode('utf-8')
    except UnicodeDecodeError as e:
        return {'path': rel, 'skipped': f'not UTF-8 at byte {e.start}'}
    digest = hashlib.sha256(data).hexdigest()
    asset = str(uuid.uuid5(NAMESPACE, f'{client.corpus}:{rel}'))
    state = client.sql(
        f"SELECT json_build_array(a.content_revision, encode(v.source_sha256, 'hex')) FROM "
        f"{_ident(client.corpus)}.assets a LEFT JOIN {_ident(client.corpus)}.versions v "
        f"ON v.version_id = a.current_version_id WHERE a.asset_id = '{asset}';")
    revision, current_digest = json.loads(state) if state else (0, None)
    if current_digest == digest:
        return {'path': rel, 'unchanged': True}
    spans = chunk_spans(data, max_span_bytes)
    staged = client.call('stage_version', {
        'asset_id': asset, 'path': rel, 'source': text, 'source_sha256': digest,
        'spans': [{'start_byte': s, 'end_byte': e} for s, e in spans],
        # Includes the base revision, so a retry after a lost response replays.
        'ingestion_key': f'{asset}@{digest}@{revision}', 'expected_revision': revision,
    })
    published = client.call('publish_version', {'version_id': staged['version_id']})
    return {'path': rel, 'version_id': staged['version_id'], 'revision': published['revision'], 'spans': len(spans)}


def _ident(name):
    return '"' + name.replace('"', '""') + '"'


def main():
    ap = argparse.ArgumentParser(description=__doc__, formatter_class=argparse.RawDescriptionHelpFormatter)
    ap.add_argument('--corpus', required=True)
    ap.add_argument('--root', required=True, type=Path)
    ap.add_argument('--init', action='store_true', help='create the corpus first')
    # 2000-byte spans keep a 1 MiB file's stage response (~95 bytes per span) under
    # the default 64 KiB max_response_bytes.
    ap.add_argument('--max-span-bytes', type=int, default=2000)
    ap.add_argument('--max-source-bytes', type=int, default=1 << 20)
    args = ap.parse_args()
    if args.max_span_bytes < 4:
        ap.error('--max-span-bytes must be at least 4 (the longest UTF-8 character)')
    if args.max_source_bytes < 1:
        ap.error('--max-source-bytes must be positive')
    client = Client(args.corpus)
    if args.init:
        client.sql(f"SELECT evidence.init_collection({_literal(args.corpus)}, '{{}}'::jsonb);")
    failures = 0
    for path in sorted(p for p in args.root.rglob('*') if p.is_file() and '.git' not in p.parts):
        try:
            result = import_file(client, args.root, path, args.max_span_bytes, args.max_source_bytes)
        except EvidenceError as e:
            failures += 1
            result = {'path': path.relative_to(args.root).as_posix(), 'error': e.sqlstate, 'reason': e.reason}
        print(json.dumps(result, ensure_ascii=False))
    return 1 if failures else 0


if __name__ == '__main__':
    sys.exit(main())
