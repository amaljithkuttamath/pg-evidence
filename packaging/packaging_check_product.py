"""Accept a product build only when tests, installation and package hashes agree."""
import hashlib
from pathlib import Path
import re
import sys


def check_results(root: Path) -> None:
    try:
        steps = (root / 'product-results.tsv').read_text().splitlines()
        required = ['tests', 'install', 'system', 'package', 'complete']
        if steps != [f'{name}\t0' for name in required]:
            raise ValueError('mandatory product steps failed, are missing or duplicated')
        tests = (root / 'step-tests.log').read_text(encoding='utf-8', errors='replace')
        if not re.search(r'test result: ok\. [1-9][0-9]* passed; 0 failed;', tests):
            raise ValueError('no passing database test evidence')
        if 'PG_EVIDENCE_SYSTEM_OK' not in (root / 'step-system.log').read_text(encoding='utf-8', errors='replace').splitlines():
            raise ValueError('system tests did not finish')
        seen = set()
        for line in (root / 'SHA256SUMS').read_text().splitlines():
            digest, name = line.split('  ', 1)
            relative = Path(name)
            if relative.is_absolute() or '..' in relative.parts or not name.startswith('package/'):
                raise ValueError('invalid artifact path')
            if name in seen or not re.fullmatch('[0-9a-f]{64}', digest):
                raise ValueError('invalid checksum manifest')
            seen.add(name)
            artifact = root / relative
            if artifact.is_symlink() or not artifact.resolve().is_relative_to(root.resolve()):
                raise ValueError('artifact escapes output directory')
            if hashlib.sha256(artifact.read_bytes()).hexdigest() != digest:
                raise ValueError(f'artifact checksum mismatch: {name}')
        mandatory = {'package/lib/pg_evidence.so', 'package/extension/pg_evidence.control',
                     'package/extension/pg_evidence--0.1.0.sql'}
        if not mandatory.issubset(seen):
            raise ValueError('required install artifacts absent from manifest')
    except (OSError, UnicodeError) as exc:
        raise ValueError(f'incomplete product evidence: {exc}') from exc


if __name__ == '__main__':
    try:
        check_results(Path(sys.argv[1]))
    except (ValueError, IndexError) as exc:
        print(f'product rejected: {exc}', file=sys.stderr)
        sys.exit(1)
    print('Product tests and install artifacts verified.')
