#!/usr/bin/env python3
"""Exercise the shell installer with native tools and disposable synthetic assets."""

import gzip
import hashlib
import http.server
import importlib.util
import io
import json
import os
from pathlib import Path
import platform
import shutil
import shlex
import subprocess
import ssl
import tarfile
import tempfile
import unittest
from unittest.mock import patch
import time
import threading

SPEC = importlib.util.spec_from_file_location("verify_release", Path(__file__).with_name("verify_release.py"))
verifier = importlib.util.module_from_spec(SPEC)
SPEC.loader.exec_module(verifier)
INSTALLER = Path(__file__).with_name("install.sh").resolve()
COMMIT = "a" * 40
NATIVE = {( "Darwin", "arm64"): "aarch64-apple-darwin",
          ("Linux", "x86_64"): "x86_64-unknown-linux-gnu"}.get((platform.system(), platform.machine()))


def fixture(directory, version, marker, minimums=None):
    directory.mkdir()
    records = []
    for target in verifier.TARGETS:
        requirement = {"kind":"macos","minimum_version":"15.0.0"} if "apple" in target else {"kind":"glibc","minimum_version":"2.17"}
        if minimums and target in minimums: requirement["minimum_version"] = minimums[target]
        binary = ('#!/bin/sh\n# fixture-minimum ' + requirement["minimum_version"] + '\nprintf "executed\\n" >> ' + str(marker) + '\nprintf "lwiki ' + version + '\\n"\n').encode()
        contents = {
            "lwiki": binary, "version.txt": ("lwiki " + version + "\n").encode(),
            "capabilities.json": json.dumps({"ok": True, "command": "capabilities"}, indent=2).encode(),
            "build-info.json": json.dumps({"schema_version": 2, "platform_requirements": requirement, "source": {"commit": COMMIT, "dirty": False},
                "target": target, "host": target, "package_version": version, "binary": "lwiki",
                "binary_sha256": verifier.digest(binary), "build_command": ["bazel", "build", "--config=release"]}, indent=2).encode(),
            "LICENSE": b"MIT fixture license\n", "THIRD-PARTY-NOTICES": b"fixture notice\n",
            "INSTALL.txt": b"fixture instructions\n",
        }
        contents["artifact-info.txt"] = verifier.artifact_text(contents, COMMIT, version, target).encode()
        archive = directory / ("lwiki-" + version + "-" + target + ".tar.gz")
        write_archive(archive, contents)
        checksum = directory / (archive.name + ".sha256")
        checksum.write_text(verifier.digest(archive.read_bytes()) + "  " + archive.name + "\n")
        with patch.object(verifier.platforms, "inspect_bytes", return_value=requirement):
            records.append(verifier.verify_archive(archive, checksum, COMMIT, version, target))
    manifest = {"package_version": version, "source_commit": COMMIT, "targets": records}
    (directory / "release-manifest.txt").write_text(verifier.manifest_text(manifest))
    return directory


def write_archive(archive, contents, extra=None, tar_format=tarfile.USTAR_FORMAT):
    with tarfile.open(archive, "w:gz", format=tar_format) as bundle:
        for name, data in contents.items():
            member = tarfile.TarInfo(name)
            member.size = len(data)
            member.mode = 0o755 if name == "lwiki" else 0o644
            if tar_format == tarfile.PAX_FORMAT:
                member.pax_headers = {"comment": "must refuse hidden PAX metadata"}
            bundle.addfile(member, io.BytesIO(data))
        if extra is not None:
            bundle.addfile(extra, io.BytesIO(b"x") if extra.size else None)


@unittest.skipUnless(os.name == "posix" and os.getuid() != 0 and NATIVE in verifier.TARGETS, "requires a supported native user account")
class InstallTests(unittest.TestCase):
    def setUp(self):
        self.temp = tempfile.TemporaryDirectory(prefix="lwiki-install-test-")
        self.addCleanup(self.temp.cleanup)
        self.root = Path(self.temp.name).resolve()
        self.prefix = self.root / "prefix with spaces café"
        self.mock_platform_tools()
        self.marker = self.root / "executed"
        self.assets = fixture(self.root / "release-0.3.0", "0.3.0", self.marker)
        self.new_assets = fixture(self.root / "release-0.3.1", "0.3.1", self.marker)

    def mock_platform_tools(self):
        # Synthetic shell executables need explicitly mocked native inspection.
        # The fake derives its response from a fixture marker in the selected bytes.
        self.tool_environment = self.tool_wrapper("otool", """
file=; for argument do file=$argument; done
minimum=$(sed -n 's/^# fixture-minimum //p' "$file")
printf '%s\\n' 'Mach header' 'MH_MAGIC_64 ARM64 ALL 0x00 EXECUTE 1 32' 'Load command 0' 'cmd LC_BUILD_VERSION' 'platform 1' "minos ${MOCK_ARTIFACT_MINIMUM:-$minimum}" 'sdk 26.5'
""", directory="platform-tools")
        self.tool_environment = self.tool_wrapper("readelf", """
file=; for argument do file=$argument; done
minimum=$(sed -n 's/^# fixture-minimum //p' "$file")
printf '%s\\n' 'Class: ELF64' 'Data: little endian' 'Type: DYN' 'Machine: Advanced Micro Devices X86-64' 'Version needs section .gnu.version_r contains 1 entry:' "Name: GLIBC_${MOCK_ARTIFACT_MINIMUM:-$minimum} Flags: none Version: 2"
""", directory="platform-tools")

    def host_environment(self, version):
        if platform.system() == "Darwin":
            environment = self.tool_wrapper("sw_vers", "printf '%s\\n' \"$MOCK_HOST_VERSION\"\n")
        else:
            environment = self.tool_wrapper("getconf", "printf 'glibc %s\\n' \"$MOCK_HOST_VERSION\"\n")
        environment["MOCK_HOST_VERSION"] = version
        return environment

    def run_install(self, *args, success=True, env=None):
        result = subprocess.run(["/bin/sh", str(INSTALLER), "--prefix", str(self.prefix), *args],
                                text=True, capture_output=True, timeout=20, env=env or self.tool_environment)
        if success:
            self.assertEqual(result.returncode, 0, result.stdout + result.stderr)
        else:
            self.assertNotEqual(result.returncode, 0, result.stdout + result.stderr)
        return result

    def install(self, version="0.3.0", assets=None, **kwargs):
        return self.run_install("--version", version, "--from-dir", str(assets or self.assets), **kwargs)

    def native_archive(self, assets=None, version="0.3.0"):
        return (assets or self.assets) / ("lwiki-" + version + "-" + NATIVE + ".tar.gz")

    def active(self):
        return (self.prefix / "bin/lwiki").resolve().read_bytes()

    def rehash_archive(self, archive=None):
        archive = archive or self.native_archive()
        checksum = archive.with_name(archive.name + ".sha256")
        checksum.write_text(verifier.digest(archive.read_bytes()) + "  " + archive.name + "\n")
        path = archive.parent / "release-manifest.txt"
        lines = path.read_text().splitlines()
        for i, line in enumerate(lines):
            if line.startswith("asset " + NATIVE + " "):
                fields = line.split()
                fields[2] = verifier.digest(archive.read_bytes())
                fields[5] = str(archive.stat().st_size)
                lines[i] = " ".join(fields)
        path.write_text("\n".join(lines) + "\n")

    def test_native_install_update_rollback_uninstall_preserves_sentinel(self):
        self.prefix.mkdir()
        sentinel = self.prefix / "user vault and config.txt"
        sentinel.write_bytes(b"keep arbitrary user bytes\x00\xff")
        self.install()
        first = self.active()
        self.assertIn(b"lwiki 0.3.0", first)
        subprocess.run([str(self.prefix / "bin/lwiki"), "--version"], check=True, capture_output=True)
        self.install("0.3.1", self.new_assets)
        self.assertNotEqual(self.active(), first)
        self.run_install("--rollback", "0.3.0")
        self.assertEqual(self.active(), first)
        self.run_install("--uninstall")
        self.assertFalse((self.prefix / "bin/lwiki").is_symlink())
        self.assertFalse((self.prefix / ".lwiki-install/current").is_symlink())
        self.assertEqual(len(list((self.prefix / ".lwiki-install/versions").iterdir())), 2)
        self.assertEqual(sentinel.read_bytes(), b"keep arbitrary user bytes\x00\xff")
        self.install()
        self.assertEqual(self.active(), first)

    def test_dry_run_verifies_exact_target_without_prefix_or_execution(self):
        result = self.run_install("--version", "0.3.0", "--from-dir", str(self.assets), "--dry-run")
        self.assertIn("target=" + NATIVE, result.stdout)
        self.assertIn("commit=" + COMMIT, result.stdout)
        self.assertFalse(self.prefix.exists())
        self.assertFalse(self.marker.exists())

    def test_older_host_refuses_install_and_dry_run_before_execution_or_activation(self):
        self.install()
        active, executed = self.active(), self.marker.read_bytes()
        older = "14.9.9" if platform.system()=="Darwin" else "2.16"
        environment = self.host_environment(older)
        for dry in (False, True):
            options = ["--version","0.3.1","--from-dir",str(self.new_assets)] + (["--dry-run"] if dry else [])
            result = self.run_install(*options, success=False, env=environment)
            self.assertIn("or newer; your system has", result.stderr)
            self.assertEqual(self.active(), active)
            self.assertEqual(self.marker.read_bytes(), executed)

    def test_older_host_refuses_only_selected_rollback_and_keeps_compatible_current(self):
        higher = "26.1.0" if platform.system()=="Darwin" else "2.40"
        lower = "25.9.9" if platform.system()=="Darwin" else "2.39"
        supported = "26.2.0" if platform.system()=="Darwin" else "2.41"
        self.assets = fixture(self.root / "higher-floor", "0.3.0", self.marker, {NATIVE:higher})
        environment = self.host_environment(supported)
        self.install(env=environment)
        self.install("0.3.1", self.new_assets, env=environment)
        active, executed = self.active(), self.marker.read_bytes()
        environment["MOCK_HOST_VERSION"] = lower
        for options in (["--rollback","0.3.0"], ["--rollback","0.3.0","--dry-run"]):
            result = self.run_install(*options, success=False, env=environment)
            self.assertIn(higher, result.stderr)
            self.assertEqual(self.active(), active)
            self.assertEqual(self.marker.read_bytes(), executed)

    def test_platform_versions_compare_numerically_and_reject_malformed_hosts(self):
        equal = "15.0" if platform.system()=="Darwin" else "2.17"
        self.run_install("--version","0.3.0","--from-dir",str(self.assets),"--dry-run", env=self.host_environment(equal))
        newer = "15.10.0" if platform.system()=="Darwin" else "2.100"
        self.run_install("--version","0.3.0","--from-dir",str(self.assets),"--dry-run", env=self.host_environment(newer))
        for malformed in ("015.0", "15.", "15.0beta", "15.0\n15.0", "15.0\n"):
            with self.subTest(malformed=malformed):
                self.run_install("--version","0.3.0","--from-dir",str(self.assets),"--dry-run", success=False, env=self.host_environment(malformed))
        self.assertFalse(self.prefix.exists())
        self.assertFalse(self.marker.exists())

    def test_native_executable_floor_cannot_disagree_with_metadata_on_dry_run(self):
        environment = dict(self.tool_environment)
        environment["MOCK_ARTIFACT_MINIMUM"] = "26.5" if platform.system()=="Darwin" else "2.39"
        result = self.run_install("--version","0.3.0","--from-dir",str(self.assets),"--dry-run",success=False,env=environment)
        self.assertIn("release metadata declares", result.stderr)
        self.assertFalse(self.prefix.exists())
        self.assertFalse(self.marker.exists())

    def test_explicit_version_and_unsupported_arguments(self):
        for args in ((), ("--version", "latest"), ("--version", "../0.3.0"), ("--version", "0.3.0", "--uninstall"), ("--unknown",)):
            with self.subTest(args=args):
                self.run_install(*args, success=False)
        self.assertFalse(self.prefix.exists())

    def test_same_version_reinstall_is_verified_and_idempotent(self):
        self.install()
        current = os.readlink(self.prefix / ".lwiki-install/current")
        self.install()
        self.assertEqual(os.readlink(self.prefix / ".lwiki-install/current"), current)
        self.assertEqual(len(list((self.prefix / ".lwiki-install/versions").iterdir())), 1)

    def test_manifest_adversarial_records_refused_before_execution(self):
        path = self.assets / "release-manifest.txt"
        original = path.read_bytes()
        values = [original + b"junk\n", original[:-1], original.replace(b"\n", b"\r\n"),
                  original.replace(b"commit " + COMMIT.encode(), b"commit abc123"),
                  original.replace(b"version 0.3.0", b"version 0.0.0"),
                  original.replace(b"asset aarch64-apple-darwin", b"asset x86_64-unknown-linux-gnu"),
                  original.replace(b"lwiki.release-manifest.v2", b"lwiki.release-manifest.v2\x00")]
        fields = original.decode().splitlines()
        native = next(i for i, line in enumerate(fields) if line.startswith("asset " + NATIVE))
        for index, value in ((2, "A" * 64), (4, "999999999999999999999999"), (5, "0001")):
            changed = fields.copy(); row = changed[native].split(); row[index] = value; changed[native] = " ".join(row)
            values.append(("\n".join(changed) + "\n").encode())
        for value in values:
            with self.subTest(value=value):
                path.write_bytes(value)
                self.install(success=False)
                self.assertFalse(self.marker.exists())
                self.assertFalse((self.prefix / ".lwiki-install/current").is_symlink())
        path.write_bytes(original)

    def test_archive_hash_failure_refuses_before_parsing_or_execution(self):
        archive = self.native_archive()
        archive.write_bytes(archive.read_bytes() + b"modified")
        self.install(success=False)
        self.assertFalse(self.marker.exists())

    def test_tar_parser_adversarial_parity(self):
        archive = self.native_archive()
        original = archive.read_bytes()
        contents = verifier.read_ustar(archive)
        cases = []
        for name, kind in (("../outside", tarfile.REGTYPE), ("/absolute", tarfile.REGTYPE),
                           ("lwiki", tarfile.REGTYPE), ("new\nline", tarfile.REGTYPE),
                           ("link", tarfile.SYMTYPE), ("link", tarfile.LNKTYPE), ("fifo", tarfile.FIFOTYPE)):
            cases.append((name, kind))
        for name, kind in cases:
            with self.subTest(name=name, kind=kind):
                member = tarfile.TarInfo(name); member.type = kind; member.linkname = "../outside"; member.size = 1 if kind == tarfile.REGTYPE else 0
                write_archive(archive, contents, extra=member)
                self.rehash_archive()
                with self.assertRaises(verifier.VerificationError):
                    verifier.read_ustar(archive)
                self.install(success=False)
                self.assertFalse(self.marker.exists())
        write_archive(archive, contents, tar_format=tarfile.PAX_FORMAT)
        self.rehash_archive()
        with self.assertRaises(verifier.VerificationError): verifier.read_ustar(archive)
        self.install(success=False)
        raw = gzip.decompress(original)
        archive.write_bytes(gzip.compress(raw + raw))
        self.rehash_archive()
        with self.assertRaises(verifier.VerificationError): verifier.read_ustar(archive)
        self.install(success=False)
        self.assertFalse(self.marker.exists())

    def test_forged_metadata_and_duplicate_json_refused(self):
        archive = self.native_archive()
        original_contents = verifier.read_ustar(archive)
        for filename, changed in (("build-info.json", {"target": "wrong"}),
                                  ("capabilities.json", {"ok": False}),
                                  ("build-info.json", {"build_command": ["build", "--config=debug"]})):
            contents = dict(original_contents)
            info = json.loads(contents[filename]); info.update(changed); contents[filename] = json.dumps(info, indent=2).encode()
            contents["artifact-info.txt"] = verifier.artifact_text(contents, COMMIT, "0.3.0", NATIVE).encode()
            write_archive(archive, contents); self.rehash_archive()
            self.install(success=False)
            self.assertFalse(self.marker.exists())
        contents = dict(original_contents)
        contents["capabilities.json"] = b'{"ok":false,"ok":true,"command":"capabilities"}'
        contents["artifact-info.txt"] = verifier.artifact_text(contents, COMMIT, "0.3.0", NATIVE).encode()
        write_archive(archive, contents); self.rehash_archive(); self.install(success=False)
        self.assertFalse(self.marker.exists())

    def test_user_modified_generation_receipt_launch_and_unknown_paths_refuse(self):
        self.install()
        initial = self.active()
        generation = (self.prefix / ".lwiki-install/current").resolve()
        for name in ("lwiki", "receipt", "LICENSE"):
            path = generation / name; original = path.read_bytes(); path.write_bytes(original + b"tampered")
            self.run_install("--rollback", "0.3.0", success=False)
            self.run_install("--uninstall", success=False)
            path.write_bytes(original)
        unknown = generation / "unknown.txt"; unknown.write_text("keep")
        self.run_install("--uninstall", success=False)
        self.assertEqual(unknown.read_text(), "keep"); unknown.unlink()
        launch = self.prefix / "bin/lwiki"; launch.unlink(); launch.symlink_to("unknown")
        self.install(success=False)
        self.assertEqual(os.readlink(launch), "unknown")
        self.assertEqual((generation / "lwiki").read_bytes(), initial)

    def test_occupied_lock_is_never_deleted(self):
        self.install()
        lock = self.prefix / ".lwiki-install/.lock"; lock.mkdir(); (lock / "owner").write_text("unknown owner\n")
        self.install(success=False)
        self.assertEqual((lock / "owner").read_text(), "unknown owner\n")

    def test_unknown_prefix_launch_and_symlink_components_refuse(self):
        self.prefix.mkdir(); (self.prefix / "bin").mkdir(); launch = self.prefix / "bin/lwiki"; launch.write_text("user file")
        self.install(success=False)
        self.assertEqual(launch.read_text(), "user file")
        launch.unlink(); self.prefix.chmod(0o777)
        self.install(success=False)
        self.prefix.chmod(0o700)
        alias = self.root / "alias"; alias.symlink_to(self.prefix, target_is_directory=True)
        self.run_install("--prefix", str(alias), "--version", "0.3.0", "--from-dir", str(self.assets), success=False)

    def test_native_no_target_directory_rename(self):
        old = self.root / "old"; new = self.root / "new"; old.mkdir(); new.mkdir()
        current = self.root / "current"; current.symlink_to(old, target_is_directory=True)
        candidate = self.root / "candidate"; candidate.symlink_to(new, target_is_directory=True)
        flag = "-h" if platform.system() == "Darwin" else "-T"
        subprocess.run(["/bin/mv", flag, str(candidate), str(current)], check=True)
        self.assertEqual(current.resolve(), new)
        self.assertFalse((old / "candidate").exists())

    def tool_wrapper(self, name, body, directory="tools"):
        tools = self.root / directory
        tools.mkdir(exist_ok=True)
        wrapper = tools / name
        wrapper.write_text("#!/bin/sh\nset -eu\n" + body)
        wrapper.chmod(0o755)
        environment = dict(getattr(self, "tool_environment", os.environ))
        environment["PATH"] = str(tools) + os.pathsep + environment["PATH"]
        return environment

    def test_process_cuts_at_publication_boundaries_and_safe_retry(self):
        for boundary in ("generation", "current", "launch"):
            for when in ("before", "after"):
                with self.subTest(boundary=boundary, when=when):
                    self.prefix = self.root / (boundary + " " + when + " café")
                    cut_marker = self.root / (boundary + "-" + when + ".cut")
                    environment = self.tool_wrapper("mv", '''
last=; previous=
for argument do previous=$last; last=$argument; done
boundary=
case "$last" in */versions/*) [ "${previous##*/}" != generation ] || boundary=generation ;; */current) boundary=current ;; */bin/lwiki) boundary=launch ;; esac
cut=false
if [ "$boundary" = "$CUT_BOUNDARY" ] && [ ! -f "$CUT_MARKER" ]; then cut=true; : > "$CUT_MARKER"; fi
if [ "$cut" = true ] && [ "$CUT_WHEN" = before ]; then kill -TERM "$PPID"; exit 143; fi
/bin/mv "$@"
if [ "$cut" = true ] && [ "$CUT_WHEN" = after ]; then kill -TERM "$PPID"; exit 143; fi
''')
                    environment.update(CUT_BOUNDARY=boundary, CUT_WHEN=when, CUT_MARKER=str(cut_marker))
                    self.install(success=False, env=environment)
                    self.assertTrue(cut_marker.exists())
                    managed = self.prefix / ".lwiki-install"
                    self.assertFalse((managed / ".lock").exists())
                    # A fresh interrupted install can have a complete current
                    # generation and no launch link. Retrying reconciles it.
                    self.install(env=environment)
                    self.assertIn(b"lwiki 0.3.0", self.active())

    def test_bootstrap_cuts_before_after_owner_versions_and_publish_retry(self):
        for boundary in ("owner", "versions", "bootstrap"):
            for when in ("before", "after"):
                with self.subTest(boundary=boundary, when=when):
                    self.prefix = self.root / ("bootstrap " + boundary + " " + when + " café")
                    marker = self.root / ("bootstrap-" + boundary + "-" + when + ".cut")
                    common = '''
last=; previous=
for argument do previous=$last; last=$argument; done
boundary=
case "$TOOL:$last" in mkdir:*/.lwiki-install/versions) boundary=versions ;; mv:*/.lwiki-install/.owner) boundary=owner ;; esac
if [ "$TOOL" = mv ] && [ "${previous##*/}" = .lwiki-install ] && [ "$last" = "$INSTALL_PREFIX" ]; then boundary=bootstrap; fi
cut=false
if [ "$boundary" = "$CUT_BOUNDARY" ] && [ ! -f "$CUT_MARKER" ]; then cut=true; : > "$CUT_MARKER"; fi
if [ "$cut" = true ] && [ "$CUT_WHEN" = before ]; then kill -TERM "$PPID"; exit 143; fi
/bin/"$TOOL" "$@"
if [ "$cut" = true ] && [ "$CUT_WHEN" = after ]; then kill -TERM "$PPID"; exit 143; fi
'''
                    self.tool_wrapper("mkdir", "TOOL=mkdir\n" + common)
                    environment = self.tool_wrapper("mv", "TOOL=mv\n" + common)
                    environment.update(CUT_BOUNDARY=boundary, CUT_WHEN=when, CUT_MARKER=str(marker), INSTALL_PREFIX=str(self.prefix))
                    self.install(success=False, env=environment)
                    self.assertTrue(marker.exists())
                    managed = self.prefix / ".lwiki-install"
                    if managed.exists():
                        self.assertTrue((managed / ".owner").is_file())
                        self.assertTrue((managed / "versions").is_dir())
                    self.assertFalse((managed / ".lock").exists())
                    # The ordinary retry has no timing wrapper or recovery flag.
                    self.install()
                    self.assertIn(b"lwiki 0.3.0", self.active())

    def owner_only_bootstrap(self):
        self.prefix.mkdir()
        managed = self.prefix / ".lwiki-install"
        managed.mkdir(mode=0o700)
        marker = "lwiki.install-owner.v1 {} {}\n".format(
            os.getuid(), hashlib.sha256(str(self.prefix).encode()).hexdigest())
        (managed / ".owner").write_text(marker)
        (managed / ".owner").chmod(0o600)
        return managed

    def test_exact_prior_owner_only_bootstrap_completes_on_ordinary_retry(self):
        managed = self.owner_only_bootstrap()
        marker = (managed / ".owner").read_bytes()
        self.install()
        self.assertEqual((managed / ".owner").read_bytes(), marker)
        self.assertIn(b"lwiki 0.3.0", self.active())

    def test_unknown_incomplete_bootstrap_paths_are_preserved_and_refused(self):
        managed = self.owner_only_bootstrap()
        for name in ("unknown.txt", ".lock", ".stage", "current", "version-looking"):
            with self.subTest(name=name):
                unknown = managed / name
                unknown.write_bytes(b"unknown user bytes\x00\xff")
                self.install(success=False)
                self.assertEqual(unknown.read_bytes(), b"unknown user bytes\x00\xff")
                self.assertFalse((managed / "versions").exists())
                unknown.unlink()
        (managed / ".owner").unlink()
        self.install(success=False)
        self.assertEqual(list(managed.iterdir()), [])
        self.assertFalse(self.marker.exists())

    def test_changed_bootstrap_cleanup_keeps_unknown_material(self):
        environment = self.tool_wrapper("mkdir", '''
last=; for argument do last=$argument; done
case "$last" in */.lwiki-install/versions)
  printf 'foreign bytes' > "${last%/versions}/unknown.txt"
  kill -TERM "$PPID"; exit 143
;; esac
exec /bin/mkdir "$@"
''')
        self.install(success=False, env=environment)
        retained = list(self.prefix.glob(".lwiki-bootstrap.*/.lwiki-install/unknown.txt"))
        self.assertEqual(len(retained), 1)
        self.assertEqual(retained[0].read_bytes(), b"foreign bytes")
        self.install()
        self.assertEqual(retained[0].read_bytes(), b"foreign bytes")

    def test_native_bootstrap_publication_collision_preserves_both_trees(self):
        parent = self.root / "publication parent"; parent.mkdir()
        sources = []
        for name in ("first", "second"):
            holder = self.root / name; holder.mkdir()
            candidate = holder / ".lwiki-install"; candidate.mkdir()
            (candidate / ".owner").write_text(name)
            (candidate / "versions").mkdir()
            sources.append(candidate)
        subprocess.run(["/bin/mv", str(sources[0]), str(parent)], check=True)
        result = subprocess.run(["/bin/mv", str(sources[1]), str(parent)], capture_output=True)
        self.assertNotEqual(result.returncode, 0)
        self.assertEqual((parent / ".lwiki-install/.owner").read_text(), "first")
        self.assertEqual((sources[1] / ".owner").read_text(), "second")
        self.assertFalse((parent / ".lwiki-install/.lwiki-install").exists())

    def test_concurrent_install_refuses_occupied_real_lock(self):
        entered = self.root / "entered"
        release = self.root / "release"
        environment = self.tool_wrapper("mv", '''
last=; previous=
for argument do previous=$last; last=$argument; done
if [ "${previous##*/}" = generation ]; then
  : > "$ENTERED"; count=0
  while [ ! -f "$RELEASE" ]; do count=$((count+1)); [ "$count" -le 100 ] || exit 1; sleep .05; done
fi
exec /bin/mv "$@"
''')
        environment.update(ENTERED=str(entered), RELEASE=str(release))
        command = ["/bin/sh", str(INSTALLER), "--prefix", str(self.prefix), "--version", "0.3.0", "--from-dir", str(self.assets)]
        first = subprocess.Popen(command, stdout=subprocess.PIPE, stderr=subprocess.PIPE, text=True, env=environment)
        self.addCleanup(lambda: first.poll() is not None or first.kill())
        deadline = time.monotonic() + 10
        while not entered.exists() and time.monotonic() < deadline:
            time.sleep(.02)
        self.assertTrue(entered.exists())
        owner = (self.prefix / ".lwiki-install/.lock/owner").read_bytes()
        result = self.install(success=False)
        self.assertIn("installation lock exists", result.stderr)
        self.assertEqual((self.prefix / ".lwiki-install/.lock/owner").read_bytes(), owner)
        release.write_text("continue")
        stdout, stderr = first.communicate(timeout=20)
        self.assertEqual(first.returncode, 0, stdout + stderr)
        self.assertIn(b"lwiki 0.3.0", self.active())

    def test_changed_version_bytes_refused_before_new_executable_runs(self):
        self.install()
        before = self.marker.read_bytes()
        active = self.active()
        archive = self.native_archive()
        contents = verifier.read_ustar(archive)
        contents["lwiki"] += b"# changed immutable version bytes\n"
        info = json.loads(contents["build-info.json"])
        info["binary_sha256"] = verifier.digest(contents["lwiki"])
        contents["build-info.json"] = json.dumps(info, indent=2).encode()
        contents["artifact-info.txt"] = verifier.artifact_text(contents, COMMIT, "0.3.0", NATIVE).encode()
        write_archive(archive, contents); self.rehash_archive()
        manifest = self.assets / "release-manifest.txt"
        rows = manifest.read_text().splitlines()
        for index, row in enumerate(rows):
            if row.startswith("asset " + NATIVE):
                fields = row.split(); fields[3] = info["binary_sha256"]; fields[4] = str(len(contents["lwiki"])); rows[index] = " ".join(fields)
        manifest.write_text("\n".join(rows) + "\n")
        result = self.install(success=False)
        self.assertIn("already exists with different bytes", result.stderr)
        self.assertEqual(self.marker.read_bytes(), before)
        self.assertEqual(self.active(), active)

    def test_prefix_swap_detected_at_activation_recheck(self):
        outside = self.root / "outside"; outside.mkdir()
        sentinel = outside / "sentinel"; sentinel.write_text("keep")
        moved = self.root / "moved prefix"
        environment = self.tool_wrapper("ln", '''
/bin/ln "$@"
last=; for argument do last=$argument; done
case "$last" in */next-current)
  /bin/mv "$PREFIX_TO_SWAP" "$MOVED_PREFIX"
  /bin/ln -s "$OUTSIDE" "$PREFIX_TO_SWAP"
;; esac
''')
        environment.update(PREFIX_TO_SWAP=str(self.prefix), MOVED_PREFIX=str(moved), OUTSIDE=str(outside))
        result = self.install(success=False, env=environment)
        self.assertIn("not a regular directory", result.stderr)
        self.assertEqual(list(outside.iterdir()), [sentinel])
        self.assertFalse((moved / ".lwiki-install/current").exists())
        self.assertFalse(self.prefix.resolve() == moved)

    def test_declared_member_bound_and_compressed_bound_refused(self):
        archive = self.native_archive()
        raw = bytearray(gzip.decompress(archive.read_bytes()))
        # The first member is lwiki. Change only its physical size declaration.
        raw[124:136] = ("{:011o}".format(verifier.MAX_ARCHIVE + 1)).encode() + b"\x00"
        raw[148:156] = b"        "
        checksum = sum(raw[:512]); raw[148:156] = ("{:06o}\0 ".format(checksum)).encode()
        archive.write_bytes(gzip.compress(raw)); self.rehash_archive()
        with self.assertRaises(verifier.VerificationError): verifier.read_ustar(archive)
        self.install(success=False)
        with archive.open("wb") as target:
            target.truncate(verifier.MAX_ARCHIVE + 1)
        self.install(success=False)
        self.assertFalse(self.marker.exists())

    def test_gzip_output_limit_checks_producer_failure(self):
        archive = self.native_archive()
        with gzip.open(archive, "wb", compresslevel=1) as stream:
            block = bytes(1024 * 1024)
            for _ in range(193): stream.write(block)
        self.rehash_archive()
        result = self.install(success=False)
        self.assertIn("bounded command failed", result.stderr)
        self.assertFalse(self.marker.exists())
        self.assertFalse((self.prefix / ".lwiki-install/current").exists())

    def test_invalid_utf8_and_nul_json_refused_before_execution(self):
        archive = self.native_archive()
        original = verifier.read_ustar(archive)
        for data in (b'{"ok":true,"command":"capabilities","extra":"\xff"}',
                     b'{"ok":true,"command":"capabilities","extra":"\x00"}'):
            contents = dict(original); contents["capabilities.json"] = data
            contents["artifact-info.txt"] = verifier.artifact_text(contents, COMMIT, "0.3.0", NATIVE).encode()
            write_archive(archive, contents); self.rehash_archive(); self.install(success=False)
            self.assertFalse(self.marker.exists())


@unittest.skipUnless(NATIVE in verifier.TARGETS and shutil.which("curl") and shutil.which("openssl"), "requires native curl and openssl")
class NativeDownloadTests(unittest.TestCase):
    def test_https_chunked_size_bound_and_truncated_transfer_status(self):
        observed = subprocess.run(["curl", "-q", "--version"], text=True, capture_output=True, check=True).stdout.split()[1]
        if tuple(int(part) for part in observed.split(".")[:2]) < (8, 4):
            self.skipTest("installer online mode requires curl >=8.4")
        with tempfile.TemporaryDirectory() as temporary:
            root = Path(temporary)
            config = root / "certificate.cnf"
            config.write_text('[req]\ndistinguished_name=dn\nx509_extensions=extensions\nprompt=no\n[dn]\nCN=localhost\n[extensions]\nsubjectAltName=DNS:localhost\nbasicConstraints=critical,CA:true\n')
            certificate, key = root / "certificate.pem", root / "key.pem"
            subprocess.run(["openssl", "req", "-x509", "-newkey", "rsa:2048", "-nodes", "-days", "1", "-config", str(config), "-keyout", str(key), "-out", str(certificate)], check=True, capture_output=True)

            class Handler(http.server.BaseHTTPRequestHandler):
                protocol_version = "HTTP/1.1"

                def log_message(self, *args):
                    pass

                def do_GET(self):
                    self.send_response(200)
                    if self.path == "/chunked":
                        self.send_header("Transfer-Encoding", "chunked"); self.end_headers()
                        try:
                            for _ in range(32):
                                self.wfile.write(b"1000\r\n" + b"x" * 4096 + b"\r\n"); self.wfile.flush()
                            self.wfile.write(b"0\r\n\r\n")
                        except (BrokenPipeError, ConnectionResetError, ssl.SSLError):
                            pass
                    else:
                        self.send_header("Content-Length", "4096"); self.end_headers()
                        self.wfile.write(b"x" * 512); self.wfile.flush(); self.close_connection = True

            server = http.server.ThreadingHTTPServer(("127.0.0.1", 0), Handler)
            context = ssl.SSLContext(ssl.PROTOCOL_TLS_SERVER)
            context.load_cert_chain(certificate, key)
            server.socket = context.wrap_socket(server.socket, server_side=True)
            thread = threading.Thread(target=server.serve_forever, daemon=True); thread.start()
            try:
                for path, limit, code in (("chunked", 1024, 63), ("truncated", 4096, 18)):
                    destination = root / path
                    result = subprocess.run(["curl", "-q", "--fail", "--silent", "--show-error", "--location", "--proto", "=https", "--proto-redir", "=https", "--connect-timeout", "15", "--max-time", "10", "--max-filesize", str(limit), "--cacert", str(certificate), "https://localhost:{}/{}".format(server.server_port, path), "-o", str(destination)], capture_output=True, text=True, timeout=15)
                    self.assertEqual(result.returncode, code, result.stderr)
                    self.assertLessEqual(destination.stat().st_size if destination.exists() else 0, limit)
            finally:
                server.shutdown(); server.server_close(); thread.join(timeout=2)


if __name__ == "__main__":
    unittest.main()
