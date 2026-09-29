"""Qualification orchestration failures, using fake tools and disposable paths."""

import os
from pathlib import Path
import subprocess
import tempfile
import unittest


SCRIPT = Path(__file__).resolve().with_name("qualify-local.sh")


@unittest.skipIf(os.name == "nt", "the full qualification script is Unix-only")
class QualificationTests(unittest.TestCase):
    def setUp(self):
        self.temp = tempfile.TemporaryDirectory()
        self.addCleanup(self.temp.cleanup)
        self.root = Path(self.temp.name).resolve()
        self.output = self.root / "evidence with spaces"
        self.tools = self.root / "tools"
        self.tools.mkdir()
        for name, source in {
            "bazelisk": "#!/bin/sh\necho 'fixture Bazel failure' >&2\nexit 23\n",
        }.items():
            path = self.tools / name
            path.write_text(source)
            path.chmod(0o755)
        self.env = dict(os.environ, PATH=str(self.tools) + os.pathsep + os.environ["PATH"])

    def run_script(self):
        return subprocess.run(["bash", str(SCRIPT), str(self.output)], env=self.env,
                              capture_output=True, text=True, check=False)

    def test_failure_keeps_log_and_reports_stage_and_physical_path(self):
        result = self.run_script()
        self.assertEqual(result.returncode, 23)
        self.assertIn("failed during fmt", result.stderr)
        self.assertIn(str(self.output), result.stderr)
        self.assertEqual((self.output / "fmt.log").read_text(), "fixture Bazel failure\n")
        self.assertFalse((self.output / "tests.log").exists())
        self.assertFalse((self.output / "qualification.txt").exists())

    def test_existing_evidence_is_preserved(self):
        self.output.mkdir()
        sentinel = self.output / "fmt.log"
        sentinel.write_text("old evidence\n")
        result = self.run_script()
        self.assertNotEqual(result.returncode, 0)
        self.assertEqual(sentinel.read_text(), "old evidence\n")
        self.assertEqual(list(self.output.iterdir()), [sentinel])

    def test_existing_empty_directory_is_refused(self):
        self.output.mkdir()
        self.assertNotEqual(self.run_script().returncode, 0)
        self.assertEqual(list(self.output.iterdir()), [])


if __name__ == "__main__":
    unittest.main()
