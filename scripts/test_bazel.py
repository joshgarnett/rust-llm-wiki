"""Native platform and environment boundaries without starting Bazel."""

import importlib.util
from pathlib import Path
import unittest
from unittest.mock import patch


SPEC = importlib.util.spec_from_file_location("bazel_wrapper", Path(__file__).with_name("bazel.py"))
wrapper = importlib.util.module_from_spec(SPEC)
SPEC.loader.exec_module(wrapper)


class BazelWrapperTests(unittest.TestCase):
    def test_environment_keeps_tool_discovery_and_omits_credentials_and_injection(self):
        source = {"HOME": "/fixture", "PATH": "/tools", "INCLUDE": "native headers",
                  "GITHUB_TOKEN": "fixture", "OPENAI_API_KEY": "fixture", "RUSTFLAGS": "injected",
                  "CARGO_BUILD_TARGET": "wasm32-unknown-unknown", "HTTPS_PROXY": "credential-url",
                  "VCTOOLSINSTALLDIR": "compiler", "WINDOWSSDKDIR": "sdk", "WINDOWSSDKVERSION": "version"}
        self.assertEqual(wrapper.build_environment(source),
                         {"HOME": "/fixture", "PATH": "/tools", "INCLUDE": "native headers",
                          "VCTOOLSINSTALLDIR": "compiler", "WINDOWSSDKDIR": "sdk", "WINDOWSSDKVERSION": "version"})
        self.assertIn("GITHUB_TOKEN", source)

    def test_native_platforms_are_explicit(self):
        for (system, machine), expected in wrapper.NATIVE_TARGETS.items():
            with self.subTest(system=system, machine=machine):
                with patch.object(wrapper.platform, "system", return_value=system), \
                     patch.object(wrapper.platform, "machine", return_value=machine.upper()):
                    self.assertEqual(wrapper.native_target(), expected)

    def test_unknown_platform_is_rejected(self):
        with patch.object(wrapper.platform, "system", return_value="unknown"):
            with self.assertRaisesRegex(ValueError, "unsupported native"):
                wrapper.native_target()

    def test_run_native_flags_precede_application_arguments_and_preserve_failure(self):
        with patch.object(wrapper.sys, "argv", ["bazel.py", "--", "run", "//:lwiki", "--", "--version"]), \
             patch.object(wrapper.shutil, "which", return_value="/tools/bazelisk"), \
             patch.object(wrapper, "native_target", return_value=("macos_aarch64", "aarch64-apple-darwin")), \
             patch.object(wrapper.subprocess, "run") as run:
            run.return_value.returncode = 23
            self.assertEqual(wrapper.main(), 23)
        command = run.call_args.args[0]
        self.assertEqual(command[-6:], ["run", "//:lwiki", "--platforms=//platforms:macos_aarch64",
                                       "--host_platform=//platforms:macos_aarch64", "--", "--version"])


if __name__ == "__main__":
    unittest.main()
