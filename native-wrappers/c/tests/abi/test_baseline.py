import argparse
import hashlib
import io
import json
import pathlib
import tarfile
import tempfile
import unittest
from unittest import mock

import baseline
from baseline import parse_version, safe_extract, select_baseline, verify_checksum


class BaselineSelectionTests(unittest.TestCase):
    def test_prerelease_and_first_lines_have_no_baseline(self):
        self.assertIsNone(select_baseline("0.1.0-alpha", ["rumqttc-c-v0.1.0"]))
        self.assertIsNone(select_baseline("0.1.0", []))
        self.assertIsNone(select_baseline("0.2.0", ["rumqttc-c-v0.1.9"]))
        self.assertIsNone(select_baseline("1.0.0", ["rumqttc-c-v0.9.9"]))

    def test_patch_prerelease_is_checked_before_publication(self):
        self.assertEqual(
            select_baseline("0.1.2-alpha.1", ["rumqttc-c-v0.1.0", "rumqttc-c-v0.1.1"]),
            "rumqttc-c-v0.1.1",
        )

    def test_prestable_patch_uses_latest_release_in_minor_line(self):
        tags = ["rumqttc-c-v0.1.0", "rumqttc-c-v0.1.2", "rumqttc-c-v0.2.0"]
        self.assertEqual(select_baseline("0.1.3", tags), "rumqttc-c-v0.1.2")

    def test_stable_release_uses_latest_lower_release_in_major_line(self):
        tags = ["rumqttc-c-v1.0.0", "rumqttc-c-v1.2.1", "rumqttc-c-v2.0.0"]
        self.assertEqual(select_baseline("1.3.0", tags), "rumqttc-c-v1.2.1")

    def test_promised_line_fails_closed_without_baseline(self):
        with self.assertRaises(RuntimeError):
            select_baseline("0.1.1", [])
        with self.assertRaises(RuntimeError):
            select_baseline("1.1.0", [])

    def test_version_parser_rejects_unknown_shapes(self):
        self.assertEqual(parse_version("0.1.0-alpha.1"), (0, 1, 0))
        self.assertIsNone(parse_version("main"))

    def test_checksum_mismatch_fails_closed(self):
        with tempfile.TemporaryDirectory() as directory:
            root = pathlib.Path(directory)
            archive = root / "archive.tar.gz"
            checksum = root / "archive.tar.gz.sha256"
            archive.write_bytes(b"released bytes")
            checksum.write_text(f"{hashlib.sha256(b'released bytes').hexdigest()}  archive.tar.gz\n")
            verify_checksum(archive, checksum)
            checksum.write_text(f"{'0' * 64}  archive.tar.gz\n")
            with self.assertRaises(RuntimeError):
                verify_checksum(archive, checksum)

    def test_archive_path_traversal_is_rejected(self):
        with tempfile.TemporaryDirectory() as directory:
            root = pathlib.Path(directory)
            archive = root / "archive.tar.gz"
            with tarfile.open(archive, "w:gz") as bundle:
                member = tarfile.TarInfo("../outside")
                member.size = 1
                bundle.addfile(member, io.BytesIO(b"x"))
            with self.assertRaises(RuntimeError):
                safe_extract(archive, root / "output")

    def test_successful_resolution_clears_stale_no_baseline_state(self):
        with tempfile.TemporaryDirectory() as directory:
            output = pathlib.Path(directory) / "baseline"
            output.mkdir()
            (output / "baseline.json").write_text("stale", encoding="utf-8")
            no_baseline_args = argparse.Namespace(
                version="0.1.0",
                platform="linux-x86_64",
                output=str(output),
                skip_attestation=True,
            )
            with mock.patch.object(baseline, "request_json", return_value=[]):
                self.assertEqual(baseline.resolve(no_baseline_args), 0)
            self.assertTrue((output / "no-baseline").is_file())
            self.assertFalse((output / "baseline.json").exists())
            (output / "extracted" / "stale-file").parent.mkdir()
            (output / "extracted" / "stale-file").write_text("stale", encoding="utf-8")

            release = {
                "tag_name": "rumqttc-c-v0.1.0",
                "assets": [
                    {"name": "rumqttc-c-linux-x86_64.tar.gz", "browser_download_url": "archive"},
                    {"name": "rumqttc-c-linux-x86_64.tar.gz.sha256", "browser_download_url": "checksum"},
                ],
            }

            def fake_download(_url, destination):
                destination.write_bytes(b"fixture")

            def fake_extract(_archive, destination):
                root = destination / "rumqttc-c-linux-x86_64"
                (root / "include").mkdir(parents=True)
                (root / "include" / "rumqttc.h").write_text("fixture", encoding="utf-8")
                (root / "lib").mkdir()
                (root / "lib" / "librumqttc.so.0.1").write_bytes(b"fixture")
                contract = root / "share" / "rumqttc" / "abi-contract.json"
                contract.parent.mkdir(parents=True)
                contract.write_text(json.dumps({"package_version": "0.1.0"}), encoding="utf-8")

            baseline_args = argparse.Namespace(
                version="0.1.1",
                platform="linux-x86_64",
                output=str(output),
                skip_attestation=True,
            )
            with (
                mock.patch.object(baseline, "request_json", return_value=[release]),
                mock.patch.object(baseline, "download", side_effect=fake_download),
                mock.patch.object(baseline, "verify_checksum"),
                mock.patch.object(baseline, "safe_extract", side_effect=fake_extract),
            ):
                self.assertEqual(baseline.resolve(baseline_args), 0)

            self.assertFalse((output / "no-baseline").exists())
            self.assertTrue((output / "baseline.json").is_file())
            self.assertFalse((output / "extracted" / "stale-file").exists())


if __name__ == "__main__":
    unittest.main()
