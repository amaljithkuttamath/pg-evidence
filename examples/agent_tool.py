"""Agent tool adapter for pg-evidence: two bounded tools, no model calls.

    python3 examples/agent_tool.py schema                      # tool definitions (JSON)
    python3 examples/agent_tool.py search docs "retry semantics" [--hops]
    python3 examples/agent_tool.py cite docs <evidence_id>

`search` sends one composed plan (lexical hits, optionally one hop of
relations, union) so the agent gets bounded, deduplicated evidence in one call.
The database response obeys max_response_bytes. Framework serialization and
tool-message overhead must be accounted for separately by the caller.
`cite` resolves a stored evidence ID to verified bytes or its status. Your
agent framework supplies the model; this file only executes tool calls.
Connection settings come from PG* environment variables.
"""
import json
import sys
from pathlib import Path

sys.path.insert(0, str(Path(__file__).resolve().parent))
from evidence_client import Client, EvidenceError  # noqa: E402

TOOLS = [
    {
        'name': 'evidence_search',
        'description': 'Search current evidence. Returns excerpts with evidence_id, path and byte offsets; '
                       'cite answers by evidence_id. Set hops=true to include directly related evidence.',
        'input_schema': {
            'type': 'object',
            'properties': {
                'corpus': {'type': 'string'},
                'query': {'type': 'string', 'maxLength': 1024},
                'limit': {'type': 'integer', 'minimum': 1, 'maximum': 20, 'default': 8},
                'hops': {'type': 'boolean', 'default': False},
                'path_prefix': {'type': 'string'},
            },
            'required': ['corpus', 'query'],
        },
    },
    {
        'name': 'evidence_cite',
        'description': 'Resolve an evidence_id to its exact retained text, or report it as '
                       'historical, retired, staged, purged or not_found.',
        'input_schema': {
            'type': 'object',
            'properties': {'corpus': {'type': 'string'}, 'evidence_id': {'type': 'string', 'format': 'uuid'}},
            'required': ['corpus', 'evidence_id'],
        },
    },
]


def search_plan(query, limit=8, hops=False, path_prefix=None, excerpt_bytes=400):
    hits = {'id': 'hits', 'op': 'lexical', 'query': query, 'limit': limit}
    if path_prefix:
        hits['filter'] = {'path_prefix': path_prefix}
    if not hops:
        return {'nodes': [hits], 'output': 'hits', 'excerpt_bytes': excerpt_bytes}
    return {
        'nodes': [hits,
                  {'id': 'related', 'op': 'neighbors', 'from': 'hits', 'status': ['current'], 'limit': limit},
                  {'id': 'all', 'op': 'union', 'inputs': ['hits', 'related']}],
        'output': 'all', 'excerpt_bytes': excerpt_bytes,
    }


def run_tool(name, args):
    """Executes one tool call; errors become structured tool results."""
    client = Client(args['corpus'])
    try:
        if name == 'evidence_search':
            return client.query(search_plan(args['query'], args.get('limit', 8), args.get('hops', False),
                                            args.get('path_prefix')))
        if name == 'evidence_cite':
            return client.resolve(args['evidence_id'])
        return {'error': 'unknown_tool', 'tool': name}
    except EvidenceError as e:
        return {'error': e.sqlstate, 'reason': e.reason, 'detail': e.detail}


def main(argv):
    if len(argv) >= 1 and argv[0] == 'schema':
        print(json.dumps(TOOLS, indent=2))
        return 0
    if len(argv) >= 3 and argv[0] == 'search':
        result = run_tool('evidence_search', {'corpus': argv[1], 'query': argv[2], 'hops': '--hops' in argv})
    elif len(argv) == 3 and argv[0] == 'cite':
        result = run_tool('evidence_cite', {'corpus': argv[1], 'evidence_id': argv[2]})
    else:
        print(__doc__, file=sys.stderr)
        return 2
    print(json.dumps(result, ensure_ascii=False, indent=2))
    return 1 if 'error' in result else 0


if __name__ == '__main__':
    sys.exit(main(sys.argv[1:]))
