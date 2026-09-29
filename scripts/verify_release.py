#!/usr/bin/env python3
"""Validate six native candidate artifacts without extracting or executing them."""

import argparse
import hashlib
import json
from pathlib import Path
import re
import sys
import tarfile


TARGETS = (
    "x86_64-unknown-linux-gnu",
    "aarch64-unknown-linux-gnu",
    "x86_64-apple-darwin",
    "aarch64-apple-darwin",
    "x86_64-pc-windows-msvc",
    "aarch64-pc-windows-msvc",
)


class VerificationError(ValueError):
    pass


def sha256_stream(source):
    digest = hashlib.sha256()
    for block in iter(lambda: source.read(1024 * 1024), b""):
        digest.update(block)
    return digest.hexdigest()


def member_text(bundle, member):
    if member.size > 1024 * 1024:
        raise VerificationError("oversized metadata member: " + member.name)
    with bundle.extractfile(member) as source:
        return source.read().decode("utf-8")


def verify_archive(archive, checksum, commit, version, target):
    for path in (archive, checksum):
        if path.is_symlink() or not path.is_file():
            raise VerificationError("asset must be a regular file: " + str(path))
    checksum_text = checksum.read_text(encoding="utf-8")
    match = re.fullmatch(r"([0-9a-f]{64})  " + re.escape(archive.name) + r"\n", checksum_text)
    if match is None:
        raise VerificationError("checksum must name exactly its archive: " + str(checksum))
    with archive.open("rb") as source:
        if sha256_stream(source) != match.group(1):
            raise VerificationError("archive checksum mismatch: " + str(archive))

    binary = "lwiki.exe" if "windows" in target else "lwiki"
    expected_members = {binary, "version.txt", "capabilities.json", "build-info.json"}
    with tarfile.open(archive, "r:gz") as bundle:
        members = bundle.getmembers()
        if (len(members) != len(expected_members)
                or {member.name for member in members} != expected_members
                or any(not member.isreg() for member in members)):
            raise VerificationError("archive members must be exactly four unique regular files: " + str(archive))
        by_name = {member.name: member for member in members}
        if "windows" not in target and by_name[binary].mode & 0o111 == 0:
            raise VerificationError("Unix binary lacks executable permissions: " + str(archive))
        info = json.loads(member_text(bundle, by_name["build-info.json"]))
        if not isinstance(info, dict) or type(info.get("schema_version")) is not int or info["schema_version"] != 1:
            raise VerificationError("invalid build-info schema: " + str(archive))
        source = info.get("source")
        if (not isinstance(source, dict) or source.get("dirty") is not False
                or source.get("commit") != commit):
            raise VerificationError("build-info must identify a clean expected commit: " + str(archive))
        if (info.get("target") != target or info.get("host") != target
                or info.get("package_version") != version or info.get("binary") != binary):
            raise VerificationError("build-info version, target or binary mismatch: " + str(archive))
        with bundle.extractfile(by_name[binary]) as executable:
            if sha256_stream(executable) != info.get("binary_sha256"):
                raise VerificationError("binary checksum mismatch: " + str(archive))
        if member_text(bundle, by_name["version.txt"]).strip() != "lwiki " + version:
            raise VerificationError("binary version output mismatch: " + str(archive))
        capabilities = json.loads(member_text(bundle, by_name["capabilities.json"]))
        if (not isinstance(capabilities, dict) or capabilities.get("ok") is not True
                or capabilities.get("command") != "capabilities"):
            raise VerificationError("unsuccessful capabilities output: " + str(archive))


def verify_release(root, commit, version):
    """Return twelve verified asset paths only after all six targets pass."""
    if re.fullmatch(r"[0-9a-f]{40}(?:[0-9a-f]{24})?", commit) is None:
        raise VerificationError("commit must be a full lowercase Git object ID")
    if re.fullmatch(r"[0-9]+\.[0-9]+\.[0-9]+(?:-[0-9A-Za-z.-]+)?(?:\+[0-9A-Za-z.-]+)?", version) is None:
        raise VerificationError("version must be a package version without path separators")
    root = Path(root).resolve()
    if not root.is_dir():
        raise VerificationError("artifact root is not a directory: " + str(root))
    directories = sorted(root.iterdir())
    if (len(directories) != len(TARGETS)
            or any(path.is_symlink() or not path.is_dir() for path in directories)):
        raise VerificationError("artifact root must contain exactly six artifact directories")
    expected = {"lwiki-{}-{}.tar.gz".format(version, target): target for target in TARGETS}
    verified = {}
    for directory in directories:
        archives = list(directory.rglob("*.tar.gz"))
        checksums = list(directory.rglob("*.sha256"))
        if (len(archives) != 1 or archives[0].parent != directory
                or archives[0].name not in expected):
            raise VerificationError("artifact must contain one expected native archive: " + str(directory))
        archive = archives[0]
        checksum = directory / (archive.name + ".sha256")
        if checksums != [checksum]:
            raise VerificationError("artifact must contain exactly its archive checksum: " + str(directory))
        target = expected[archive.name]
        if target in verified:
            raise VerificationError("duplicate native target: " + target)
        verify_archive(archive, checksum, commit, version, target)
        verified[target] = [str(archive), str(checksum)]
    if set(verified) != set(TARGETS):
        raise VerificationError("missing native release target")
    return [path for target in TARGETS for path in verified[target]]


def main(argv=None):
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("root", help="download directory with one subdirectory per Actions artifact")
    parser.add_argument("--commit", required=True, help="full release source commit")
    parser.add_argument("--version", required=True, help="Cargo package version")
    args = parser.parse_args(argv)
    try:
        assets = verify_release(args.root, args.commit, args.version)
    except (VerificationError, OSError, UnicodeError, json.JSONDecodeError, tarfile.TarError, EOFError) as error:
        print("release verification failed: " + str(error), file=sys.stderr)
        return 1
    print(json.dumps(assets, indent=2))
    return 0


if __name__ == "__main__":
    sys.exit(main())
