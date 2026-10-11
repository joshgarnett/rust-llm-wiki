#!/usr/bin/env python3
"""Verify the two supported native archives without executing their contents."""

import argparse
import gzip
import hashlib
import json
import importlib.util
from pathlib import Path
import re
import sys

_spec = importlib.util.spec_from_file_location("artifact_platform", Path(__file__).with_name("artifact_platform.py"))
platforms = importlib.util.module_from_spec(_spec)
_spec.loader.exec_module(platforms)

TARGETS = ("aarch64-apple-darwin", "x86_64-unknown-linux-gnu")
MEMBERS = ("lwiki", "version.txt", "capabilities.json", "build-info.json",
           "LICENSE", "THIRD-PARTY-NOTICES", "INSTALL.txt", "artifact-info.txt")
MAX_ARCHIVE = 128 * 1024 * 1024
MAX_TAR = 192 * 1024 * 1024
LIMITS = {name: 1024 * 1024 for name in MEMBERS}
LIMITS.update({"lwiki": MAX_ARCHIVE, "THIRD-PARTY-NOTICES": 16 * 1024 * 1024})
VERSION_RE = r"[0-9]+\.[0-9]+\.[0-9]+(?:-[0-9A-Za-z.-]+)?(?:\+[0-9A-Za-z.-]+)?"
COMMIT_RE = r"[0-9a-f]{40}(?:[0-9a-f]{24})?"


class VerificationError(ValueError):
    pass


def sha256_stream(source):
    digest = hashlib.sha256()
    for block in iter(lambda: source.read(1024 * 1024), b""):
        digest.update(block)
    return digest.hexdigest()


def digest(data):
    return hashlib.sha256(data).hexdigest()


def validate_identity(commit, version):
    if re.fullmatch(COMMIT_RE, commit) is None:
        raise VerificationError("commit must be a full lowercase Git object ID")
    if len(version) > 80 or re.fullmatch(VERSION_RE, version) is None:
        raise VerificationError("version must be a bounded package version")


def octal(field):
    if re.fullmatch(rb"[0-7]+[\x00 ]*", field) is None:
        raise VerificationError("nonstandard tar numeric field")
    return int(field.rstrip(b"\x00 "), 8)


def read_ustar(archive):
    """Accept only our fixed regular-file USTAR subset, including physical headers.

    tarfile hides extension headers and trailing concatenated archives. Reading the
    physical headers keeps the installer and release verifier on the same grammar.
    """
    if archive.stat().st_size > MAX_ARCHIVE:
        raise VerificationError("oversized compressed archive")
    with gzip.open(archive, "rb") as source:
        data = source.read(MAX_TAR + 1)
    if len(data) > MAX_TAR or len(data) % 512:
        raise VerificationError("oversized or unaligned decoded tar")
    result, offset = {}, 0
    while offset + 512 <= len(data):
        header = data[offset:offset + 512]
        if header == bytes(512):
            if offset + 1024 > len(data) or len(data) - offset > 10240 or any(data[offset:]):
                raise VerificationError("invalid tar terminator or trailing archive")
            if set(result) != set(MEMBERS):
                raise VerificationError("archive must contain eight unique regular root files")
            return result
        if (header[257:265] != b"ustar\x0000" or header[156:157] != b"0"
                or any(header[157:257]) or any(header[345:])):
            raise VerificationError("unsupported tar type, extension, link or prefix")
        name_field = header[:100]
        name, separator, padding = name_field.partition(b"\x00")
        if not separator or any(padding):
            raise VerificationError("invalid tar name padding")
        try:
            name = name.decode("ascii")
        except UnicodeError as error:
            raise VerificationError("non-ASCII tar name") from error
        if name not in MEMBERS or name in result:
            raise VerificationError("archive must contain eight unique regular root files")
        checksum = octal(header[148:156])
        if sum(header[:148]) + 8 * 32 + sum(header[156:]) != checksum:
            raise VerificationError("invalid tar header checksum")
        mode, size = octal(header[100:108]), octal(header[124:136])
        if mode & ~0o777 or (name == "lwiki" and mode & 0o111 == 0):
            raise VerificationError("invalid member mode or Unix binary lacks executable permissions")
        if size == 0 or size > LIMITS[name]:
            raise VerificationError("empty or oversized archive member: " + name)
        start = offset + 512
        end = start + size
        padded_end = start + ((size + 511) // 512) * 512
        if padded_end > len(data) or any(data[end:padded_end]):
            raise VerificationError("truncated member or nonzero tar padding")
        result[name] = data[start:end]
        offset = padded_end
    raise VerificationError("missing tar terminator")


def strict_json(data):
    def pairs(items):
        result = {}
        for key, value in items:
            if key in result:
                raise VerificationError("duplicate JSON key: " + key)
            result[key] = value
        return result
    return json.loads(data.decode("utf-8"), object_pairs_hook=pairs,
                      parse_constant=lambda value: (_ for _ in ()).throw(VerificationError("invalid JSON constant")))


def artifact_text(contents, commit, version, target, clean=True):
    info = strict_json(contents["build-info.json"])
    if not isinstance(info, dict):
        raise VerificationError("invalid build-info document")
    requirements = platforms.validate(info.get("platform_requirements"), target)
    lines = ["lwiki.artifact-info.v2", "version " + version, "commit " + commit,
             "target " + target, "source_clean " + ("true" if clean else "false")]
    lines.append("requirement {} {}".format(requirements["kind"], requirements["minimum_version"]))
    for name in MEMBERS[:-1]:
        lines.append("file {} {} {}".format(name, digest(contents[name]), len(contents[name])))
    return "\n".join(lines) + "\n"


def verify_archive(archive, checksum, commit, version, target):
    validate_identity(commit, version)
    if target not in TARGETS:
        raise VerificationError("unsupported native target")
    archive, checksum = Path(archive), Path(checksum)
    for path in (archive, checksum):
        if path.is_symlink() or not path.is_file():
            raise VerificationError("asset must be a regular file: " + str(path))
    if checksum.stat().st_size > 512:
        raise VerificationError("oversized checksum")
    match = re.fullmatch(r"([0-9a-f]{64})  " + re.escape(archive.name) + r"\n",
                         checksum.read_text(encoding="ascii"))
    if match is None:
        raise VerificationError("checksum must name exactly its archive")
    with archive.open("rb") as source:
        archive_digest = sha256_stream(source)
    if archive_digest != match.group(1):
        raise VerificationError("archive checksum mismatch")
    contents = read_ustar(archive)
    if contents["artifact-info.txt"] != artifact_text(contents, commit, version, target).encode("ascii"):
        raise VerificationError("artifact metadata or member hash mismatch")
    info = strict_json(contents["build-info.json"])
    if not isinstance(info, dict) or type(info.get("schema_version")) is not int or info["schema_version"] != 2:
        raise VerificationError("invalid build-info schema")
    source = info.get("source")
    if not isinstance(source, dict) or source.get("dirty") is not False or source.get("commit") != commit:
        raise VerificationError("build-info must identify a clean expected commit")
    if (info.get("target") != target or info.get("host") != target
            or info.get("package_version") != version or info.get("binary") != "lwiki"):
        raise VerificationError("build-info version, target or binary mismatch")
    if digest(contents["lwiki"]) != info.get("binary_sha256"):
        raise VerificationError("binary checksum mismatch")
    requirements = platforms.validate(info.get("platform_requirements"), target)
    command = info.get("build_command")
    if not isinstance(command, list) or "build" not in command or "--config=release" not in command:
        raise VerificationError("build-info must record the optimized release build")
    if contents["version.txt"] != ("lwiki " + version + "\n").encode("ascii"):
        raise VerificationError("binary version output mismatch")
    capabilities = strict_json(contents["capabilities.json"])
    if (not isinstance(capabilities, dict) or capabilities.get("ok") is not True
            or capabilities.get("command") != "capabilities"):
        raise VerificationError("unsuccessful capabilities output")
    if platforms.inspect_bytes(contents["lwiki"], target) != requirements:
        raise VerificationError("packaged executable platform requirements disagree with metadata")
    return {"target": target, "archive_sha256": archive_digest,
            "archive_bytes": archive.stat().st_size, "binary_sha256": digest(contents["lwiki"]),
            "binary_bytes": len(contents["lwiki"]),
            "platform_requirements": requirements,
            "files": {name: {"sha256": digest(contents[name]), "bytes": len(contents[name])} for name in MEMBERS}}


def manifest_text(manifest):
    lines = ["lwiki.release-manifest.v2", "version " + manifest["package_version"],
             "commit " + manifest["source_commit"]]
    for asset in manifest["targets"]:
        requirements = platforms.validate(asset.get("platform_requirements"), asset["target"])
        lines.append("asset {} {} {} {} {} {} {}".format(asset["target"], asset["archive_sha256"],
                     asset["binary_sha256"], asset["binary_bytes"], asset["archive_bytes"],
                     requirements["kind"], requirements["minimum_version"]))
    return "\n".join(lines) + "\n"


def verify_release(root, commit, version, manifest_output=None):
    validate_identity(commit, version)
    root = Path(root).resolve()
    if not root.is_dir():
        raise VerificationError("artifact root is not a directory")
    directories = sorted(root.iterdir())
    if (len(directories) != len(TARGETS)
            or any(path.is_symlink() or not path.is_dir() for path in directories)):
        raise VerificationError("artifact root must contain exactly two artifact directories")
    expected = {"lwiki-{}-{}.tar.gz".format(version, target): target for target in TARGETS}
    verified, records = {}, {}
    for directory in directories:
        archives = list(directory.rglob("*.tar.gz"))
        checksums = list(directory.rglob("*.sha256"))
        if len(archives) != 1 or archives[0].parent != directory or archives[0].name not in expected:
            raise VerificationError("artifact must contain one expected native archive")
        archive = archives[0]
        checksum = directory / (archive.name + ".sha256")
        if checksums != [checksum]:
            raise VerificationError("artifact must contain exactly its archive checksum")
        target = expected[archive.name]
        if target in verified:
            raise VerificationError("duplicate native target")
        records[target] = verify_archive(archive, checksum, commit, version, target)
        verified[target] = [str(archive), str(checksum)]
    if set(verified) != set(TARGETS):
        raise VerificationError("missing native release target")
    if manifest_output is not None:
        destination = Path(manifest_output)
        if destination.exists() or destination.is_symlink():
            raise VerificationError("manifest output must be a fresh directory")
        destination.mkdir(parents=True)
        manifest = {"schema_version": "lwiki.release-manifest.v2", "package_version": version,
                    "source_commit": commit, "targets": [records[target] for target in TARGETS]}
        (destination / "release-manifest.json").write_text(json.dumps(manifest, indent=2) + "\n", encoding="utf-8")
        (destination / "release-manifest.txt").write_text(manifest_text(manifest), encoding="ascii")
    return [path for target in TARGETS for path in verified[target]]


def main(argv=None):
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("root", help="directory with one subdirectory per native artifact")
    parser.add_argument("--commit", required=True, help="full release source commit")
    parser.add_argument("--version", required=True, help="Cargo package version")
    parser.add_argument("--manifest-output", help="fresh directory for verified JSON and installer manifests")
    args = parser.parse_args(argv)
    try:
        assets = verify_release(args.root, args.commit, args.version, args.manifest_output)
    except (VerificationError, platforms.PlatformError, OSError, UnicodeError, json.JSONDecodeError, EOFError) as error:
        print("release verification failed: " + str(error), file=sys.stderr)
        return 1
    print(json.dumps(assets, indent=2))
    return 0


if __name__ == "__main__":
    sys.exit(main())
