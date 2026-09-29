#!/usr/bin/env python3
"""Run the pinned Bazel on an explicit native platform with a small environment."""

import argparse
import os
from pathlib import Path
import platform
import shutil
import subprocess
import sys


NATIVE_TARGETS = {
    ("Darwin", "arm64"): ("macos_aarch64", "aarch64-apple-darwin"),
    ("Darwin", "x86_64"): ("macos_x86_64", "x86_64-apple-darwin"),
    ("Linux", "aarch64"): ("linux_aarch64", "aarch64-unknown-linux-gnu"),
    ("Linux", "x86_64"): ("linux_x86_64", "x86_64-unknown-linux-gnu"),
    ("Windows", "arm64"): ("windows_aarch64", "aarch64-pc-windows-msvc"),
    ("Windows", "amd64"): ("windows_x86_64", "x86_64-pc-windows-msvc"),
}
ENVIRONMENT_KEYS = {
    "HOME", "PATH", "TMPDIR", "TMP", "TEMP", "USER", "LOGNAME", "SHELL",
    "LANG", "LC_ALL", "LC_CTYPE", "SYSTEMROOT", "SystemRoot", "WINDIR",
    "COMSPEC", "ComSpec", "PATHEXT", "USERPROFILE", "LOCALAPPDATA", "APPDATA",
    "CARGO_HOME", "RUSTUP_HOME", "BAZELISK_HOME", "CARGO_BAZEL_REPIN",
    "BAZEL_VC", "BAZEL_VC_FULL_VERSION", "BAZEL_WINSDK_FULL_VERSION",
    "INCLUDE", "LIB", "LIBPATH", "VCINSTALLDIR", "VCToolsInstallDir",
    "WindowsSdkDir", "WindowsSDKVersion",
    "PROGRAMFILES", "PROGRAMFILES(X86)", "PROGRAMW6432", "PROGRAMDATA", "SYSTEMDRIVE",
}


def native_target():
    key = (platform.system(), platform.machine().lower())
    if key not in NATIVE_TARGETS:
        raise ValueError("unsupported native build host: " + repr(key))
    return NATIVE_TARGETS[key]


def build_environment(source):
    # In particular, do not put ambient provider/GitHub tokens into build events.
    allowed = {key.upper() for key in ENVIRONMENT_KEYS}
    return {key: value for key, value in source.items() if key.upper() in allowed}


def main():
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("--print-target", action="store_true")
    parser.add_argument("arguments", nargs=argparse.REMAINDER)
    args = parser.parse_args()
    native, triple = native_target()
    if args.print_target:
        print(triple)
        return 0
    arguments = args.arguments
    if arguments[:1] == ["--"]:
        arguments = arguments[1:]
    if not arguments:
        parser.error("a Bazel command is required after --")
    executable = shutil.which("bazelisk")
    if executable is None:
        parser.error("install Bazelisk first; see docs/builds.md")
    root = Path(__file__).resolve().parent.parent
    output_root = Path(os.environ.get("LWIKI_BAZEL_OUTPUT_ROOT", root / ".build-cache/bazel")).resolve()
    command = [executable, "--nosystem_rc", "--nohome_rc", "--output_user_root=" + str(output_root)]
    if arguments[0] in {"build", "test", "cquery", "aquery", "run", "coverage", "info", "fetch"}:
        boundary = arguments.index("--") if "--" in arguments else len(arguments)
        arguments[boundary:boundary] = ["--platforms=//platforms:" + native, "--host_platform=//platforms:" + native]
    command.extend(arguments)
    return subprocess.run(command, cwd=root, env=build_environment(os.environ), check=False).returncode


if __name__ == "__main__":
    try:
        sys.exit(main())
    except (OSError, ValueError) as error:
        print(str(error), file=sys.stderr)
        sys.exit(1)
