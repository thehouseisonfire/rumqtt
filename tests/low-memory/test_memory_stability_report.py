import csv
import importlib.util
import tempfile
import unittest
from pathlib import Path

MODULE_PATH = Path(__file__).with_name("memory_stability_report.py")
SPEC = importlib.util.spec_from_file_location("memory_stability_report", MODULE_PATH)
if SPEC is None or SPEC.loader is None:
    raise ImportError(f"cannot load spec from {MODULE_PATH}")
report = importlib.util.module_from_spec(SPEC)
SPEC.loader.exec_module(report)


class AnalyzeValuesTests(unittest.TestCase):
    def test_flat_memory(self):
        result = report.analyze_values([8_000_000] * 20)
        self.assertEqual(result["growth_bytes"], 0)
        self.assertAlmostEqual(result["projected_positive_trend_bytes"], 0)

    def test_small_noisy_memory_is_stable(self):
        values = [8_000_000 + offset for offset in (0, 4096, -4096, 8192, 0) * 4]
        result = report.analyze_values(values)
        self.assertLessEqual(result["growth_bytes"], result["allowed_growth_bytes"])
        self.assertLessEqual(result["projected_positive_trend_bytes"], result["allowed_growth_bytes"])

    def test_clearly_increasing_memory(self):
        result = report.analyze_values([8_000_000 + round_number * 100_000 for round_number in range(20)])
        self.assertGreater(result["projected_positive_trend_bytes"], result["allowed_growth_bytes"])

    def test_temporary_spike_followed_by_plateau(self):
        values = [8_000_000] * 8 + [11_000_000] + [8_100_000] * 11
        result = report.analyze_values(values)
        self.assertLessEqual(result["growth_bytes"], result["allowed_growth_bytes"])
        self.assertLessEqual(result["projected_positive_trend_bytes"], result["allowed_growth_bytes"])

    def test_negative_growth(self):
        result = report.analyze_values([9_000_000 - round_number * 25_000 for round_number in range(20)])
        self.assertLess(result["growth_bytes"], 0)
        self.assertEqual(result["projected_positive_trend_bytes"], 0)

    def test_empty_values(self):
        with self.assertRaisesRegex(report.ReportError, "at least"):
            report.analyze_values([])

    def test_too_few_rounds(self):
        with self.assertRaisesRegex(report.ReportError, "at least 10"):
            report.analyze_values([8_000_000] * 9)


class ReadRoundsTests(unittest.TestCase):
    def test_malformed_input(self):
        with tempfile.TemporaryDirectory() as directory:
            path = Path(directory) / "rounds.csv"
            path.write_text(
                "round,median_bytes,sample_count\n1,not-an-integer,5\n",
                encoding="utf-8",
            )
            with self.assertRaisesRegex(report.ReportError, "malformed"):
                report.read_round_values(path)

    def test_valid_even_median(self):
        self.assertEqual(report.median([1, 3, 5, 7]), 4.0)

    def test_extracts_five_settled_samples(self):
        with tempfile.TemporaryDirectory() as directory:
            root = Path(directory)
            samples = root / "samples.csv"
            boundaries = root / "boundaries.csv"
            output = root / "rounds.csv"
            with samples.open("w", newline="", encoding="utf-8") as destination:
                writer = csv.writer(destination)
                writer.writerow(["elapsed_ms", "unix_ms", "memory_current_bytes"])
                for elapsed in range(0, 12_000, 100):
                    writer.writerow([elapsed, elapsed, 8_000_000 + elapsed])
            with boundaries.open("w", newline="", encoding="utf-8") as destination:
                writer = csv.writer(destination)
                writer.writerow(["kind", "round", "detected_elapsed_ms", "client_elapsed_ms"])
                for round_number in range(1, 11):
                    detected = round_number * 1_000
                    writer.writerow(["measured", round_number, detected, detected])
            report.extract_rounds(samples, boundaries, output)
            with output.open(encoding="utf-8") as source:
                rows = list(csv.DictReader(source))
            self.assertEqual(len(rows), 10)
            self.assertEqual(int(rows[0]["sample_count"]), 5)
            self.assertEqual(float(rows[0]["median_bytes"]), 8_001_500)


if __name__ == "__main__":
    unittest.main()
