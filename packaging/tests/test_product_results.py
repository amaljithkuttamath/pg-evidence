"""A failed build, test or tampered package must never be accepted."""
import hashlib
from pathlib import Path
import tempfile
import sys
import unittest

sys.path.insert(0, str(Path(__file__).resolve().parents[1]))
from packaging_check_product import check_results


class ProductResultsTests(unittest.TestCase):
    def setUp(self):
        self.tmp = tempfile.TemporaryDirectory()
        self.addCleanup(self.tmp.cleanup)
        self.root = Path(self.tmp.name)
        (self.root / 'product-results.tsv').write_text(
            'tests\t0\ninstall\t0\nsystem\t0\npackage\t0\ncomplete\t0\n')
        (self.root / 'step-tests.log').write_text('test result: ok. 12 passed; 0 failed; 0 ignored; 0 measured; 0 filtered out\n')
        (self.root / 'step-system.log').write_text('PG_EVIDENCE_SYSTEM_OK\n')
        files = ['package/lib/pg_evidence.so', 'package/extension/pg_evidence.control', 'package/extension/pg_evidence--0.1.0.sql']
        sums=[]
        for name in files:
            p=self.root/name
            p.parent.mkdir(parents=True,exist_ok=True)
            p.write_bytes(b'fixture artifact')
            sums.append(hashlib.sha256(p.read_bytes()).hexdigest()+'  '+name)
        (self.root/'SHA256SUMS').write_text('\n'.join(sums)+'\n')

    def test_accepts_verified_complete_package(self):
        check_results(self.root)

    def test_accepts_non_utf8_diagnostic_bytes(self):
        # PostgreSQL logs bytes in the database's encoding. The LATIN1
        # refusal regression legitimately writes non-UTF8 diagnostics.
        (self.root / 'step-system.log').write_bytes(b'caf\xe9\nPG_EVIDENCE_SYSTEM_OK\n')
        check_results(self.root)

    def test_rejects_failed_or_missing_step(self):
        for text in ['tests\t1\ninstall\t0\nsystem\t0\npackage\t0\ncomplete\t0\n', 'tests\t0\n']:
            with self.subTest(text=text):
                (self.root/'product-results.tsv').write_text(text)
                with self.assertRaises(ValueError): check_results(self.root)

    def test_rejects_missing_test_evidence(self):
        (self.root/'step-tests.log').write_text('build succeeded\n')
        with self.assertRaises(ValueError): check_results(self.root)

    def test_rejects_modified_or_missing_package(self):
        p=self.root/'package/lib/pg_evidence.so'
        p.write_bytes(b'changed')
        with self.assertRaises(ValueError): check_results(self.root)
        p.unlink()
        with self.assertRaises(ValueError): check_results(self.root)

    def test_rejects_path_escape(self):
        (self.root/'SHA256SUMS').write_text('0'*64+'  ../external\n')
        with self.assertRaises(ValueError): check_results(self.root)

    def test_rejects_no_artifacts(self):
        (self.root/'SHA256SUMS').write_text('')
        with self.assertRaises(ValueError): check_results(self.root)

if __name__ == '__main__': unittest.main()
