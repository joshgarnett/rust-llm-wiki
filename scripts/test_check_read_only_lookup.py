#!/usr/bin/env python3
"""Harness decision regressions; no candidate, sudo or sandbox process is run."""
import contextlib
import errno
import io
import json
from pathlib import Path
import subprocess
import tempfile
import unittest
from unittest.mock import patch

import check_read_only_lookup as check


class NativeReadQualificationTests(unittest.TestCase):
    def control(self, **changes):
        result = {"uid": 65534, "read_sha256": "readable",
                  "denied": {name: errno.EACCES for name in ("create", "modify", "chmod", "delete")}}
        result.update(changes)
        return {"exit": 0, "stdout": json.dumps(result), "stderr": ""}

    def test_denial_requires_readable_input_and_each_operation_denied(self):
        accepted = check.validate_control(self.control(), 65534, "readable")
        self.assertEqual(accepted["uid"], 65534)
        for operation in ("create", "modify", "chmod", "delete"):
            result = json.loads(self.control()["stdout"])
            result["denied"][operation] = None
            record = self.control(**result)
            with self.assertRaises(RuntimeError):
                check.validate_control(record, 65534, "readable")

    def test_absent_target_is_not_permission_denial(self):
        denied = {name: errno.EPERM for name in ("create", "modify", "chmod", "delete")}
        denied["create"] = errno.ENOENT
        with self.assertRaises(RuntimeError):
            check.validate_control(self.control(denied=denied), 65534, "readable")

    def test_wrong_identity_or_unreadable_bytes_cannot_qualify(self):
        for record in (self.control(uid=1000), self.control(read_sha256="different")):
            with self.assertRaises(RuntimeError):
                check.validate_control(record, 65534, "readable")
        record = {"exit": 1, "stdout": "", "stderr": "sudo unavailable"}
        with self.assertRaises(check.Unsupported):
            check.validate_control(record, 65534, "readable")

    def test_linux_crosses_identity_with_clean_environment_and_child_timeout(self):
        command = check.enforcement_command("Linux", ["/pinned/lwiki", "--offline"],
            {"HOME": "/isolated/home", "XDG_CONFIG_HOME": "/isolated/config"},
            sudo="/usr/bin/sudo", timeout="/usr/bin/timeout")
        self.assertEqual(command[:8], ["/usr/bin/sudo", "-n", "-u", "nobody", "--", "/usr/bin/env", "-i", "HOME=/isolated/home"])
        self.assertIn("XDG_CONFIG_HOME=/isolated/config", command)
        self.assertIn("--kill-after=2s", command)
        self.assertEqual(command[-2:], ["/pinned/lwiki", "--offline"])
        with self.assertRaises(check.Unsupported):
            check.enforcement_command("Linux", ["/pinned/lwiki"], {}, sudo=None, timeout="timeout")

    def test_mac_profile_denies_fixture_writes_and_network(self):
        protected = Path("/tmp/wiki with spaces")
        profile = check.sandbox_profile([protected])
        self.assertIn("(deny network*)", profile)
        self.assertIn("(deny file-write* (subpath ", profile)
        self.assertIn(json.dumps(str(protected.resolve())), profile)
        command = check.enforcement_command("Darwin", ["/pinned/lwiki"], {},
            sandbox="/usr/bin/sandbox-exec", profile=Path("/tmp/readonly.sb"))
        self.assertEqual(command, ["/usr/bin/sandbox-exec", "-f", "/tmp/readonly.sb", "/pinned/lwiki"])

    def test_product_commands_ignore_user_configuration_and_do_not_request_cached_scope(self):
        command = check.cli_command(Path("/pinned/lwiki"), Path("/temp/wiki"), ["context", "North Lab"])
        self.assertIn("--offline", command)
        self.assertIn("--no-user-config", command)
        self.assertIn("--json", command)
        self.assertNotIn("--no-sync", command)
        human = check.cli_command(Path("/pinned/lwiki"), Path("/temp/wiki"), ["read", "--path", "page.md"], machine=False)
        self.assertNotIn("--json", human)

    def test_synthetic_publication_and_json_stderr_are_rejected(self):
        payload = {"ok": True, "meta": {"network_used": False, "freshness": "verified_snapshot",
            "index_generation": None, "read_view": {"kind": "current_observation",
            "published_generation": None, "fingerprint": "observed", "observed_at": "utc"}},
            "data": {"snapshot": {"generation": None, "control_manifest": "manifest"}}}
        record = {"label": "context", "exit": 0, "stderr": "", "stdout": json.dumps(payload)}
        check.json_result(record, True)
        payload["data"]["snapshot"]["generation"] = 0
        with self.assertRaises(RuntimeError):
            check.json_result({**record, "stdout": json.dumps(payload)}, True)
        with self.assertRaises(RuntimeError):
            check.json_result({**record, "stderr": "Provider network: not used"}, True)

    def test_cli_ceiling_stops_before_dispatch(self):
        with tempfile.TemporaryDirectory() as temporary:
            runner = check.Runner(Path(temporary), {}, Path(temporary))
            with patch.object(check.subprocess, "run", return_value=subprocess.CompletedProcess([], 0, "{}", "")) as run:
                for index in range(check.MAX_CLI_CALLS):
                    runner.run(f"mock-{index}", ["candidate-not-executed"], cli=True)
                with self.assertRaises(RuntimeError):
                    runner.run("over-limit", ["candidate-not-executed"], cli=True)
                self.assertEqual(run.call_count, check.MAX_CLI_CALLS)

    def test_unsupported_platform_never_returns_success(self):
        with tempfile.TemporaryDirectory() as temporary:
            root = Path(temporary)
            binary = root / "binary"
            binary.write_bytes(b"not executed")
            output = root / "evidence"
            argv = ["check", "--binary", str(binary), "--output", str(output), "--repo-root", str(root)]
            with patch.object(check.sys, "argv", argv), patch.object(check, "qualify", side_effect=check.Unsupported("no enforcement")), contextlib.redirect_stdout(io.StringIO()):
                self.assertEqual(check.main(), 2)
            result = json.loads((output / "result.json").read_text())
            self.assertEqual(result["status"], "unsupported")
            self.assertFalse(result["qualification_passed"])


if __name__ == "__main__":
    unittest.main()
