#!/usr/bin/env python3
"""Offline catalogue contract checks: uv run tools/test-build-catalogue.py."""
import contextlib
import gzip
import hashlib
import importlib.util
import io
import json
import os
import struct
import subprocess
import tempfile
import unittest
import zipfile
from pathlib import Path
from unittest.mock import patch

spec = importlib.util.spec_from_file_location("catalogue", Path(__file__).with_name("build-catalogue.py"))
catalogue = importlib.util.module_from_spec(spec)
spec.loader.exec_module(catalogue)


def entry(**extra):
    return {"digest": "12" * 32, "title": "A title", "author": "An author", "year": "2026",
            "created_at": 1, "updated_at": 2, "filterAuthor": "human", "mv": "-3",
            "filters": [{"biquads": {"96000": {"b": ["1", "0", "0"], "a": ["1.9", "-0.95"]}}}], **extra}


class CatalogueContract(unittest.TestCase):
    def test_duplicate_digests_keep_first_entry_and_count_matches_listing(self):
        first = entry()
        second = entry(title="Different release, same filters")
        unique = catalogue.deduplicate_entries([first, second, entry(digest="34" * 32)])
        self.assertEqual(len(unique), 2)
        self.assertEqual(unique[0]["title"], first["title"])
        self.assertEqual(len(json.loads(catalogue.build_browser_catalogue(unique))), 2)

    def test_published_coefficients_preserve_additive_signs_and_fallback_on_count_or_nonfinite(self):
        e = entry()
        published = {e["digest"]: [{"b": ["2", "3", "4"], "a": ["1.5", "-0.9"]}]}
        record = catalogue.build_binary_record(e, 96000, published)
        self.assertEqual(record[0], 0x81)
        self.assertEqual(struct.unpack("<5f", record[5:]), (2., 3., 4., 1.5, struct.unpack("<f", struct.pack("<f", -.9))[0]))
        for bad in [[], 12, published[e["digest"]] * 2, [{"b": ["1e300", "0", "0"], "a": ["0", "0"]}], [{"b": ["nan", "0", "0"], "a": ["0", "0"]}]]:
            record = catalogue.build_binary_record(e, 96000, {e["digest"]: bad})
            self.assertEqual(record[0], 1)
            self.assertAlmostEqual(struct.unpack("<5f", record[5:])[3], 1.9)

    def test_profile_hash_and_loading_model_are_checked_before_using_entries(self):
        profile = {"schema_version": 1, "profile": "float32-96k", "rate": 96000,
                   "loading_model": "additive-feedback-decimal17-v1", "entries": {"digest": []}}
        raw = json.dumps(profile).encode()
        metadata = {"id": "float32-96k", "rate": 96000, "storage": "float32", "transport": "float32",
                    "file": "float32-96k.json", "sha256": hashlib.sha256(raw).hexdigest()}
        index = {"schema_version": 1, "profiles": [metadata]}
        with patch.object(catalogue, "fetch_json", return_value=index), patch.object(catalogue, "fetch_bytes", return_value=raw):
            self.assertEqual(catalogue.load_optimised(), {"digest": []})
            with contextlib.redirect_stdout(io.StringIO()):
                metadata["sha256"] = "0" * 64
                self.assertEqual(catalogue.load_optimised(), {})
                metadata["sha256"] = hashlib.sha256(raw).hexdigest()
                metadata["rate"] = 48000
                self.assertEqual(catalogue.load_optimised(), {})
                metadata["rate"] = 96000
                bad = json.dumps({**profile, "loading_model": "subtractive"}).encode()
                metadata["sha256"] = hashlib.sha256(bad).hexdigest()
                with patch.object(catalogue, "fetch_bytes", return_value=bad):
                    self.assertEqual(catalogue.load_optimised(), {})

    def test_listing_and_feed_share_freshness_and_numeric_years(self):
        now = 2_000_000
        cutoff = now - catalogue.TWO_WEEKS
        for created, updated, expected in [(cutoff, now, "Fresh"), (cutoff - 1, cutoff, "Updated"),
                                            (cutoff - 1, cutoff - 1, "Stale"), (0, now, "Unknown")]:
            self.assertEqual(catalogue.freshness(entry(created_at=created, updated_at=updated), now), expected)
        entries = json.loads(catalogue.build_browser_catalogue([entry(), entry(digest="34" * 32, updated_at=3)]))
        self.assertEqual(entries[0]["id"], "12" * 32)
        self.assertIsInstance(entries[0]["year"], int)
        self.assertNotIn("filters", entries[0])
        self.assertNotIn("overview", entries[0])
        self.assertEqual(catalogue.build_recent_changes(entries, 3)[0]["id"], "34" * 32)
        far_future = 3 + catalogue.RECENT_CHANGES_WINDOW_SEC + 1
        self.assertEqual(catalogue.build_recent_changes(entries, far_future), [])
        self.assertEqual(catalogue.build_filter_metadata([entry(), entry(year="2025")])["years"], [2026, 2025])
        self.assertEqual(catalogue.build_filter_metadata([entry()])["filterauthors"], ["human"])
        self.assertEqual(catalogue.formatted_title(entry(content_type="TV", season="1", episode="1,2,3,5,7,8")), "A title S1E1-3, 5, 7-8")

    def test_shim_adapts_catalogue_transport_without_wrapping_api_fetch(self):
        shim = catalogue.SHIM_PATH.read_text()
        self.assertNotIn("window.fetch =", shim)
        self.assertNotIn("processEntry", shim)
        self.assertNotIn("/version.txt", shim)
        self.assertNotIn("raw.githubusercontent.com", shim)
        self.assertIn("/overviews.json.gz", shim)

    def test_pinned_wheel_hash_license_and_single_shim_source(self):
        wheel = io.BytesIO()
        with zipfile.ZipFile(wheel, "w") as zf:
            zf.writestr("ezbeq/ui/index.html", "<html><head></head></html>")
            zf.writestr("ezbeq/ui/main.js", "// upstream UI, unchanged")
            zf.writestr("ezbeq-2.12.4.dist-info/licenses/LICENSE", "upstream MIT license")
        wheel_bytes = wheel.getvalue()
        pypi = {"info": {"version": "2.12.4"}, "urls": [{"filename": "ezbeq.whl", "url": "https://example.test/ezbeq.whl",
                 "upload_time_iso_8601": "2026-01-01T00:00:00Z"}]}
        with tempfile.TemporaryDirectory() as temp, patch.object(catalogue, "fetch_json", return_value=pypi), patch.object(catalogue, "fetch_bytes", return_value=wheel_bytes), contextlib.redirect_stdout(io.StringIO()):
            output = Path(temp)
            with self.assertRaisesRegex(RuntimeError, "sha256 mismatch"):
                catalogue.download_ui(output)
            with patch.object(catalogue, "UI_WHEEL_SHA256", hashlib.sha256(wheel_bytes).hexdigest()):
                catalogue.download_ui(output)
            self.assertEqual((output / "ui/main.js").read_text(), "// upstream UI, unchanged")
            self.assertEqual((output / "ui/ezbeq-shim.js").read_bytes(), catalogue.SHIM_PATH.read_bytes())
            self.assertEqual((output / "licenses/ezbeq-LICENSE.txt").read_text(), "upstream MIT license")
            self.assertEqual((output / "ui/index.html").read_text().count("ezbeq-shim.js"), 1)

    def test_repo_stamp_ignores_the_build_output_but_not_other_changes(self):
        with tempfile.TemporaryDirectory() as tmp:
            repo = Path(tmp)
            env = {"GIT_AUTHOR_DATE": "2026-10-31T13:29:30Z", "GIT_COMMITTER_DATE": "2026-10-31T13:29:30Z",
                   "GIT_AUTHOR_NAME": "t", "GIT_AUTHOR_EMAIL": "t@example.com",
                   "GIT_COMMITTER_NAME": "t", "GIT_COMMITTER_EMAIL": "t@example.com"}
            (repo / "tools").mkdir()
            (repo / "tools/build-catalogue.py").write_text("")
            for args in (["init", "-q"], ["add", "."], ["commit", "-q", "-m", "x"]):
                subprocess.run(["git", *args], cwd=repo, check=True, env={**os.environ, **env})
            (repo / "site").mkdir()
            (repo / "site/catalogue.bin").write_bytes(b"built")
            with patch.object(catalogue, "__file__", str(repo / "tools/build-catalogue.py")), \
                    patch.object(catalogue, "current_build_stamp", return_value="WALLCLOCK"):
                self.assertEqual(catalogue.repo_build_stamp(repo / "site"), "20261031T132930Z")
                self.assertEqual(catalogue.repo_build_stamp(), "WALLCLOCK")
                (repo / "tools/build-catalogue.py").write_text("# edited")
                self.assertEqual(catalogue.repo_build_stamp(repo / "site"), "WALLCLOCK")


if __name__ == "__main__":
    unittest.main()
