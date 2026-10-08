"""Paired development SQL smoke measurements, not a release benchmark.

Each SQL file must return one JSON value. Both arms use a fresh psql connection
per observation. Timings include process startup, connection and result transfer.
Use the full benchmark protocol for server latency, memory and release claims.
Connection settings come from ordinary PG* environment variables, never artifacts.
"""
import argparse
import hashlib
import json
from pathlib import Path
import random
import statistics
import subprocess
import time


def compare(execute, baseline, candidate, repetitions, output):
    if type(repetitions) is not int or not 1 <= repetitions <= 1000:
        raise ValueError('repetitions must be between 1 and 1000')
    observations = {'baseline': [], 'candidate': []}
    rng = random.Random(101)
    for pair in range(repetitions):
        arms = [('baseline', baseline), ('candidate', candidate)]
        rng.shuffle(arms)
        pair_results = {}
        for arm, sql in arms:
            record = {'pair': pair, 'arm': arm}
            try:
                observation = execute(sql)
                result = observation['result']
                record.update(ok=True, client_ms=observation['client_ms'],
                    result_sha256=hashlib.sha256(json.dumps(result, sort_keys=True,
                        separators=(',', ':'), allow_nan=False).encode()).hexdigest())
                pair_results[arm] = record['result_sha256']
                observations[arm].append(observation['client_ms'])
            except Exception as exc:
                record.update(ok=False, error=str(exc)[:4096])
                output.write(json.dumps(record)+'\n')
                output.flush()
                raise ValueError('query failed; raw observation retained') from exc
            output.write(json.dumps(record)+'\n')
            output.flush()
        if pair_results['baseline'] != pair_results['candidate']:
            output.write(json.dumps({'pair':pair,'ok':False,'error':'result_mismatch'})+'\n')
            output.flush()
            raise ValueError('matched queries returned different evidence')
    return {arm: {'samples':len(values), 'median_client_ms':statistics.median(values),
                  'min_client_ms':min(values), 'max_client_ms':max(values)}
            for arm, values in observations.items()}


def main():
    parser=argparse.ArgumentParser(description=__doc__)
    parser.add_argument('--baseline', required=True, type=Path)
    parser.add_argument('--candidate', required=True, type=Path)
    parser.add_argument('--output', required=True, type=Path)
    parser.add_argument('--repetitions', type=int, default=5)
    parser.add_argument('--timeout-ms', type=int, default=10000)
    args=parser.parse_args()
    if not 1 <= args.timeout_ms <= 3600000:
        parser.error('timeout must be between 1 and 3600000 ms')
    sqls=[p.read_text() for p in (args.baseline,args.candidate)]
    # Refuse stale output so every artifact describes this run alone.
    args.output.mkdir(parents=True,exist_ok=False)
    def execute(sql):
        start=time.perf_counter()
        run=subprocess.run(['psql','-X','-q','-A','-t','--single-transaction',
            '-v','ON_ERROR_STOP=1','-P','pager=off',
            '-c',f'SET LOCAL statement_timeout={args.timeout_ms}',
            '-c','SET LOCAL transaction_read_only=on', '-c',sql],
            text=True,capture_output=True,timeout=args.timeout_ms/1000+5)
        elapsed=(time.perf_counter()-start)*1000
        if run.returncode:
            raise RuntimeError(run.stderr.strip())
        return {'result':json.loads(run.stdout), 'client_ms':elapsed}
    metadata={'kind':'development_smoke','client':'psql_fresh_connection_per_sample',
              'random_seed':101,'timeout_ms':args.timeout_ms,
              'sql_sha256':[hashlib.sha256(s.encode()).hexdigest() for s in sqls],
              'release_evidence':False}
    (args.output/'metadata.json').write_text(json.dumps(metadata,indent=2)+'\n')
    with (args.output/'observations.jsonl').open('w') as output:
        summary=compare(execute,*sqls,args.repetitions,output)
    (args.output/'summary.json').write_text(json.dumps(summary,indent=2)+'\n')
    print(json.dumps(summary,indent=2))


if __name__ == '__main__':
    main()
