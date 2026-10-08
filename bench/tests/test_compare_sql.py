"""Never report a speed comparison for unequal results or failed requests."""
import io
import unittest
from bench.compare_sql import compare


class CompareTests(unittest.TestCase):
    def test_unequal_results_are_recorded_and_rejected(self):
        output=io.StringIO()
        def run(sql): return {'result': [sql], 'client_ms': 1.0}
        with self.assertRaises(ValueError):
            compare(run, 'baseline', 'candidate', 2, output)
        self.assertIn('result_mismatch',output.getvalue())

    def test_json_boolean_is_not_equal_to_integer(self):
        output=io.StringIO()
        def run(sql): return {'result': True if sql=='a' else 1, 'client_ms': 1.0}
        with self.assertRaises(ValueError):
            compare(run,'a','b',1,output)

    def test_failed_query_is_retained(self):
        output=io.StringIO()
        def run(sql): raise RuntimeError('statement timeout')
        with self.assertRaises(ValueError):
            compare(run,'a','b',2,output)
        self.assertIn('statement timeout',output.getvalue())
        self.assertIn('"ok": false',output.getvalue())

    def test_equal_results_produce_both_arms_and_observation_counts(self):
        output=io.StringIO()
        def run(sql): return {'result': [{'evidence_id':'a'}], 'client_ms': 2.0 if sql=='a' else 3.0}
        summary=compare(run,'a','b',3,output)
        self.assertEqual(summary['baseline']['samples'],3)
        self.assertEqual(summary['candidate']['median_client_ms'],3.0)
        self.assertEqual(len(output.getvalue().splitlines()),6)

if __name__ == '__main__': unittest.main()
