#!/usr/bin/env python3
"""Read minimum platform versions from final executable bytes using native tools."""

import os
from pathlib import Path
import re
try:
    import resource
except ImportError:
    resource = None
import shutil
import struct
import subprocess
import tempfile


class PlatformError(ValueError):
    pass


def version(value, kind):
    pattern = r"(0|[1-9][0-9]{0,4})\.(0|[1-9][0-9]{0,4})"
    if kind in ("macos", "glibc"):
        pattern += r"(?:\.(0|[1-9][0-9]{0,4}))?"
    else:
        raise PlatformError("unsupported platform requirement kind")
    if not isinstance(value, str) or re.fullmatch(pattern, value) is None:
        raise PlatformError("invalid minimum platform version")
    parts = tuple(int(part) for part in value.split("."))
    if parts[0] == 0:
        raise PlatformError("minimum platform major version must be positive")
    return ".".join(str(part) for part in (parts + (0,) if kind == "macos" and len(parts) == 2 else parts))


def validate(requirements, target):
    kind = {"aarch64-apple-darwin": "macos", "x86_64-unknown-linux-gnu": "glibc"}.get(target)
    if (kind is None or not isinstance(requirements, dict)
            or set(requirements) != {"kind", "minimum_version"}
            or requirements.get("kind") != kind):
        raise PlatformError("missing or invalid artifact platform requirements")
    minimum = version(requirements["minimum_version"], kind)
    if minimum != requirements["minimum_version"]:
        raise PlatformError("platform requirement version must be canonical")
    return {"kind": kind, "minimum_version": minimum}


def parse_macos(text):
    blocks = re.split(r"(?m)^Load command [0-9]+\s*$", text)
    found = []
    for block in blocks:
        commands = re.findall(r"(?m)^\s*cmd (LC_[A-Z0-9_]+)\s*$", block)
        if "LC_BUILD_VERSION" in commands:
            platforms = re.findall(r"(?m)^\s*platform (\S+)\s*$", block)
            minima = re.findall(r"(?m)^\s*minos (\S+)\s*$", block)
            if platforms not in (["1"], ["macos"], ["MACOS"]) or len(minima) != 1:
                raise PlatformError("invalid macOS LC_BUILD_VERSION requirement")
            found.append(version(minima[0], "macos"))
        elif "LC_VERSION_MIN_MACOSX" in commands:
            minima = re.findall(r"(?m)^\s*version (\S+)\s*$", block)
            if len(minima) != 1:
                raise PlatformError("invalid legacy macOS deployment requirement")
            found.append(version(minima[0], "macos"))
        elif any(command.startswith("LC_VERSION_MIN_") for command in commands):
            raise PlatformError("executable is for another Apple platform")
    if len(found) != 1:
        raise PlatformError("exactly one macOS minimum deployment version is required")
    return {"kind": "macos", "minimum_version": found[0]}


def parse_glibc(text):
    in_needs, found = False, []
    for line in text.splitlines():
        if line.startswith("Version needs section "):
            in_needs = True
        elif line.startswith("Version "):
            in_needs = False
        if not in_needs:
            continue
        for name in re.findall(r"\bName:\s+(\S+)", line):
            if name.startswith("GLIBC_"):
                # ABI feature/private nodes are not safely reducible to a release number.
                found.append(version(name.removeprefix("GLIBC_"), "glibc"))
    if not found:
        raise PlatformError("ELF executable has no numeric required GLIBC version")
    return {"kind": "glibc", "minimum_version": max(found, key=lambda value: tuple(map(int, value.split("."))) + (0,) * (3 - len(value.split("."))))}


def check_header(path, target):
    with Path(path).open("rb") as source:
        header = source.read(64)
    if target == "aarch64-apple-darwin":
        if (len(header) < 32 or header[:4] != b"\xcf\xfa\xed\xfe"
                or struct.unpack_from("<I", header, 4)[0] != 0x0100000c
                or struct.unpack_from("<I", header, 12)[0] != 2):
            raise PlatformError("expected a thin ARM64 Mach-O executable")
    elif target == "x86_64-unknown-linux-gnu":
        if (len(header) < 64 or header[:7] != b"\x7fELF\x02\x01\x01"
                or struct.unpack_from("<H", header, 18)[0] != 62
                or struct.unpack_from("<H", header, 16)[0] not in (2, 3)):
            raise PlatformError("expected a little-endian x86_64 ELF executable")
    else:
        raise PlatformError("unsupported executable target")


def inspection_command(path, target):
    names = ("otool", "llvm-otool") if target == "aarch64-apple-darwin" else ("readelf", "llvm-readelf")
    tool = next((shutil.which(name) for name in names if shutil.which(name)), None)
    if tool is None:
        raise PlatformError("platform inspection requires " + " or ".join(names))
    return [tool, "-l", str(path)] if target == "aarch64-apple-darwin" else [tool, "--version-info", "--wide", str(path)]


def inspect_file(path, target, evidence_dir=None):
    if resource is None:
        raise PlatformError("native platform inspection requires macOS or Linux")
    path = Path(path)
    check_header(path, target)
    command = inspection_command(path, target)
    with tempfile.TemporaryFile() as output, tempfile.TemporaryFile() as errors:
        def bound_output():
            resource.setrlimit(resource.RLIMIT_FSIZE, (4 * 1024 * 1024, 4 * 1024 * 1024))
        try:
            result = subprocess.run(command, stdout=output, stderr=errors, check=False,
                                    timeout=30, env={**os.environ, "LC_ALL": "C"}, preexec_fn=bound_output)
        except (OSError, subprocess.TimeoutExpired) as error:
            raise PlatformError("native platform inspection could not complete") from error
        output.seek(0)
        data = output.read(4 * 1024 * 1024 + 1)
    if result.returncode or len(data) > 4 * 1024 * 1024:
        raise PlatformError("native platform inspection failed or exceeded its output bound")
    try:
        text = data.decode("utf-8")
    except UnicodeError as error:
        raise PlatformError("native platform inspection returned invalid text") from error
    requirements = parse_macos(text) if target == "aarch64-apple-darwin" else parse_glibc(text)
    validate(requirements, target)
    if evidence_dir is not None:
        Path(evidence_dir, "platform-inspection.txt").write_bytes(data)
        Path(evidence_dir, "platform-inspection-command.txt").write_text("\n".join(command) + "\n")
    return requirements


def inspect_bytes(data, target):
    with tempfile.TemporaryDirectory(prefix="lwiki-platform-") as temporary:
        path = Path(temporary, "lwiki")
        path.write_bytes(data)
        return inspect_file(path, target)


def description(requirements):
    if requirements["kind"] == "macos":
        return "Requires macOS " + requirements["minimum_version"] + " or newer."
    return "Requires GNU/Linux with glibc " + requirements["minimum_version"] + " or newer."
