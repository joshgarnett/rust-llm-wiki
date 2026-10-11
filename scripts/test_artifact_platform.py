#!/usr/bin/env python3
"""Platform-parser mechanics, plus native object inspection where tools exist."""
import importlib.util
import json
from pathlib import Path
import platform
import shutil
import struct
import subprocess
import tempfile
import unittest
import sys
from unittest.mock import patch

SPEC = importlib.util.spec_from_file_location("artifact_platform", Path(__file__).with_name("artifact_platform.py"))
requirements = importlib.util.module_from_spec(SPEC)
SPEC.loader.exec_module(requirements)

MAC = "file:\nLoad command 0\n cmd LC_BUILD_VERSION\n cmdsize 32\n platform 1\n minos 15.0\n sdk 26.5\n"
ELF = "Version symbols section .gnu.version:\n GLIBC_9.99\nVersion needs section .gnu.version_r contains 1 entry:\n File: libc.so.6\n Name: GLIBC_2.9 Flags: none Version: 2\n Name: GLIBC_2.17 Flags: none Version: 3\n Name: GLIBC_2.2.5 Flags: none Version: 4\n"

class PlatformTests(unittest.TestCase):
    def test_import_without_resource_keeps_parsers_available(self):
        portable = importlib.util.module_from_spec(SPEC)
        with patch.dict(sys.modules, {"resource": None}):
            SPEC.loader.exec_module(portable)
        self.assertEqual(portable.parse_macos(MAC)["minimum_version"], "15.0.0")
        with self.assertRaisesRegex(portable.PlatformError, "requires macOS or Linux"):
            portable.inspect_file("absent", "aarch64-apple-darwin")

    def test_macos_uses_binary_deployment_version_and_not_sdk(self):
        self.assertEqual(requirements.parse_macos(MAC), {"kind":"macos","minimum_version":"15.0.0"})
        legacy = "file:\nLoad command 0\n cmd LC_VERSION_MIN_MACOSX\n version 11.2.1\n sdk 26.5\n"
        self.assertEqual(requirements.parse_macos(legacy)["minimum_version"], "11.2.1")
        for text in (MAC.replace("platform 1", "platform 2"), MAC + MAC,
                     MAC.replace("minos 15.0", "minos latest"), "sdk 26.5\n"):
            with self.assertRaises(requirements.PlatformError): requirements.parse_macos(text)

    def test_glibc_compares_numbers_and_only_reads_required_versions(self):
        # Historic glibc nodes may have three numeric components.
        self.assertEqual(requirements.parse_glibc(ELF), {"kind":"glibc","minimum_version":"2.17"})
        for tag in ("GLIBC_PRIVATE", "GLIBC_ABI_DT_RELR", "GLIBC_2.bad"):
            with self.subTest(tag=tag), self.assertRaises(requirements.PlatformError):
                requirements.parse_glibc(ELF + " Name: " + tag + " Flags: none Version: 5\n")
        with self.assertRaises(requirements.PlatformError): requirements.parse_glibc("No version information found in this file.\n")

    def test_requirements_validate_exact_kind_shape_and_canonical_version(self):
        for requirement in (None, {}, {"kind":"macos","minimum_version":"15.0"},
                            {"kind":"macos","minimum_version":"015.0.0"},
                            {"kind":"macos","minimum_version":"15.0.0","extra":True},
                            {"kind":"glibc","minimum_version":"2.39"}):
            with self.subTest(requirement=requirement), self.assertRaises(requirements.PlatformError):
                requirements.validate(requirement, "aarch64-apple-darwin")

    def test_inspection_requires_matching_final_architecture_and_preserves_output(self):
        with tempfile.TemporaryDirectory() as temporary:
            root = Path(temporary)
            binary = root / "lwiki"
            binary.write_bytes(struct.pack("<8I", 0xfeedfacf, 0x0100000c, 0, 2, 1, 32, 0, 0))
            def tool(command, stdout, **kwargs):
                self.assertEqual(Path(command[-1]).read_bytes(), binary.read_bytes())
                self.assertEqual(kwargs["timeout"], 30)
                stdout.write(MAC.encode())
                return subprocess.CompletedProcess(command, 0)
            with patch.object(requirements, "resource", create=True), patch.object(requirements.shutil, "which", return_value="/usr/bin/otool"), patch.object(requirements.subprocess, "run", tool):
                self.assertEqual(requirements.inspect_file(binary, "aarch64-apple-darwin", root)["minimum_version"], "15.0.0")
            self.assertEqual((root / "platform-inspection.txt").read_text(), MAC)
            binary.write_bytes(b"#!/bin/sh\necho not MachO\n")
            with patch.object(requirements, "resource", create=True), self.assertRaisesRegex(requirements.PlatformError, "thin ARM64"):
                requirements.inspect_file(binary, "aarch64-apple-darwin")

    @unittest.skipUnless(platform.system() == "Darwin" and platform.machine() == "arm64" and shutil.which("clang") and shutil.which("otool"), "requires native Apple Silicon C compiler and otool")
    def test_native_macho_final_bytes_have_explicit_floor_independent_of_host_sdk(self):
        with tempfile.TemporaryDirectory() as temporary:
            binary = Path(temporary, "floor-fixture")
            subprocess.run(["clang", "-mmacosx-version-min=15.0", "-x", "c", "-o", str(binary), "-"], input=b"int main(void) { return 0; }\n", check=True, capture_output=True, timeout=30)
            self.assertEqual(requirements.inspect_file(binary, "aarch64-apple-darwin"), {"kind":"macos","minimum_version":"15.0.0"})

    @unittest.skipUnless(platform.system() == "Linux" and platform.machine() == "x86_64" and shutil.which("readelf"), "requires native GNU/Linux readelf")
    def test_native_elf_reads_actual_required_glibc_versions(self):
        binary = Path(shutil.which("true"))
        observed = requirements.inspect_file(binary, "x86_64-unknown-linux-gnu")
        self.assertEqual(observed["kind"], "glibc")
        requirements.validate(observed, "x86_64-unknown-linux-gnu")

if __name__ == "__main__": unittest.main()
