#!/usr/bin/env python3
"""Generate notices from checksum-verified, pinned Cargo registry archives."""

import argparse
import hashlib
import json
import os
from pathlib import Path, PurePosixPath
import re
import sys
import tarfile


class NoticeError(ValueError):
    pass


def strict_json(data):
    def pairs(items):
        result = {}
        for key, value in items:
            if key in result:
                raise NoticeError("duplicate notice JSON key: " + key)
            result[key] = value
        return result
    try:
        return json.loads(data, object_pairs_hook=pairs)
    except (TypeError, json.JSONDecodeError) as error:
        raise NoticeError("invalid notice JSON") from error


def supplemental_packages(provenance):
    if (not isinstance(provenance, dict) or set(provenance) != {"schema_version", "packages"}
            or type(provenance["schema_version"]) is not int or provenance["schema_version"] != 1
            or not isinstance(provenance["packages"], list)):
        raise NoticeError("unsupported supplemental notice provenance")
    result = {}
    required = {"package", "version", "crate_sha256", "upstream_commit", "commit_evidence", "files"}
    file_fields = {"file", "upstream_path", "url", "sha256"}
    for item in provenance["packages"]:
        if (not isinstance(item, dict) or set(item) != required
                or any(not isinstance(item[name], str) or not item[name] for name in required - {"files"})
                or not isinstance(item["files"], list) or not item["files"]):
            raise NoticeError("supplemental notice requires a nonempty validated file list")
        key = (item["package"], item["version"])
        if key in result:
            raise NoticeError("duplicate supplemental notice provenance")
        names, urls, paths = set(), set(), set()
        for entry in item["files"]:
            if (not isinstance(entry, dict) or set(entry) != file_fields
                    or any(not isinstance(entry[name], str) or not entry[name] for name in file_fields)):
                raise NoticeError("malformed supplemental notice file")
            if entry["file"] in names or entry["url"] in urls or entry["upstream_path"] in paths:
                raise NoticeError("duplicate supplemental notice file")
            names.add(entry["file"]); urls.add(entry["url"]); paths.add(entry["upstream_path"])
            if re.fullmatch(r"[0-9a-f]{64}", entry["sha256"]) is None:
                raise NoticeError("invalid supplemental notice checksum")
        result[key] = item
    return result


def packages(lock):
    result = []
    for block in lock.decode("utf-8").split("[[package]]")[1:]:
        fields = dict(re.findall(r'^(name|version|source|checksum) = "([^"\n]+)"$', block, re.M))
        if "source" not in fields:
            continue
        if (fields.get("source") != "registry+https://github.com/rust-lang/crates.io-index"
                or re.fullmatch(r"[A-Za-z0-9_-]+", fields.get("name", "")) is None
                or re.fullmatch(r"[A-Za-z0-9.+-]+", fields.get("version", "")) is None
                or re.fullmatch(r"[0-9a-f]{64}", fields.get("checksum", "")) is None):
            raise NoticeError("unresolved or unsupported dependency source")
        result.append(fields)
    if not result:
        raise NoticeError("no pinned registry dependencies")
    return sorted(result, key=lambda package: (package["name"], package["version"]))


def generate(repo, cache=None):
    lock = (Path(repo) / "Cargo.lock").read_bytes()
    cache = Path(cache or Path(os.environ.get("CARGO_HOME", Path.home() / ".cargo")) / "registry/cache")
    provenance = strict_json((Path(repo) / "licenses/provenance.json").read_text(encoding="utf-8"))
    supplements = supplemental_packages(provenance)
    used = set()
    output = ["Third-party notices for lwiki", "",
              "This file includes all registry dependencies pinned in Cargo.lock.",
              "It is a conservative superset, including development and platform dependencies.",
              "License texts come from checksum-verified crates or pinned upstream files.",
              "Supplemental upstream file URLs and hashes are recorded below.",
              "This list does not replace independent legal review.",
              "Cargo.lock SHA256: " + hashlib.sha256(lock).hexdigest(), ""]
    for package in packages(lock):
        basename = package["name"] + "-" + package["version"]
        archives = list(cache.glob("*/" + basename + ".crate"))
        valid = []
        for archive in archives:
            if archive.is_file() and not archive.is_symlink() and archive.stat().st_size <= 64 * 1024 * 1024:
                if hashlib.sha256(archive.read_bytes()).hexdigest() == package["checksum"]:
                    valid.append(archive)
        if not valid:
            raise NoticeError("missing checksum-verified crate: " + basename)
        with tarfile.open(valid[0], "r:gz") as bundle:
            members = bundle.getmembers()
            names = [member.name for member in members]
            if len(names) != len(set(names)):
                raise NoticeError("duplicate crate member: " + basename)
            by_name = {member.name: member for member in members}

            def read(relative, limit=2 * 1024 * 1024):
                name = basename + "/" + relative
                member = by_name.get(name)
                if member is None or not member.isreg() or member.size > limit:
                    raise NoticeError("missing or invalid license material: " + name)
                return bundle.extractfile(member).read().decode("utf-8")

            manifest = read("Cargo.toml")
            package_section = re.search(r"(?ms)^\[package\]\n(.*?)(?=^\[|\Z)", manifest)
            section = package_section.group(1) if package_section else ""
            license_match = re.search(r'^license = "([^"\n]+)"$', section, re.M)
            license_file = re.search(r'^license-file = "([^"\n]+)"$', section, re.M)
            selected = set()
            for member in members:
                path = PurePosixPath(member.name)
                if (not member.isreg() or not path.parts or path.parts[0] != basename
                        or any(part in (".", "..") for part in path.parts)):
                    continue
                lower = path.name.lower()
                if lower.startswith(("license", "copying", "copyright", "notice")):
                    selected.add("/".join(path.parts[1:]))
                if package["name"] == "r-efi" and lower == "authors":
                    selected.add("/".join(path.parts[1:]))
            if license_file:
                selected.add(license_file.group(1))
            if not license_match and not license_file:
                raise NoticeError("missing upstream license declaration: " + basename)
            supplement = supplements.get((package["name"], package["version"]))
            if not selected and supplement is None:
                raise NoticeError("missing upstream license text: " + basename)
            output.extend(["=" * 72, basename,
                           "Source: https://crates.io/crates/{}/{}".format(package["name"], package["version"]),
                           "Crate SHA256: " + package["checksum"],
                           "License declaration: " + (license_match.group(1) if license_match else "see license file"), ""])
            for relative in sorted(selected):
                text = read(relative)
                if not text.strip() or "\x00" in text:
                    raise NoticeError("empty or invalid upstream license text: " + basename)
                output.extend(["File: " + relative,
                               "File SHA256: " + hashlib.sha256(text.encode("utf-8")).hexdigest(), "", text])
            if supplement is not None:
                if selected or supplement["crate_sha256"] != package["checksum"]:
                    raise NoticeError("stale supplemental notice provenance: " + basename)
                used.add((package["name"], package["version"]))
                if re.fullmatch(r"[0-9a-f]{40}", supplement["upstream_commit"]) is None:
                    raise NoticeError("unresolved upstream notice revision")
                evidence = supplement["commit_evidence"]
                if evidence == "archive .cargo_vcs_info.json":
                    vcs = strict_json(read(".cargo_vcs_info.json"))
                    git = vcs.get("git") if isinstance(vcs, dict) else None
                    if not isinstance(git, dict) or git.get("sha1") != supplement["upstream_commit"]:
                        raise NoticeError("supplemental upstream revision does not match the pinned crate")
                elif (package["name"], package["version"], evidence) != (
                        "rustls-platform-verifier-android", "0.1.1",
                        "pinned rustls-platform-verifier 0.7.0 archive .cargo_vcs_info.json; Android crate has no VCS metadata"):
                    raise NoticeError("unresolved supplemental revision evidence")
                repository = re.search(r'^repository = "https://github.com/([^"\n]+)"$', section, re.M)
                if repository is None:
                    raise NoticeError("missing supplemental upstream repository")
                for entry in supplement["files"]:
                    filename = entry["file"]
                    if PurePosixPath(filename).name != filename or not filename.startswith(basename + "-"):
                        raise NoticeError("unsafe supplemental notice path")
                    url = entry["url"]
                    upstream_path = PurePosixPath(entry["upstream_path"])
                    if (upstream_path.is_absolute() or any(part in (".", "..") for part in upstream_path.parts)
                            or str(upstream_path) != entry["upstream_path"]):
                        raise NoticeError("unsafe supplemental upstream path")
                    expected_url = "https://raw.githubusercontent.com/{}/{}/{}".format(
                        repository.group(1), supplement["upstream_commit"], entry["upstream_path"])
                    if url != expected_url:
                        raise NoticeError("unresolved upstream notice URL")
                    data = (Path(repo) / "licenses" / filename).read_bytes()
                    if (not data.strip() or b"\x00" in data or len(data) > 1024 * 1024
                            or hashlib.sha256(data).hexdigest() != entry["sha256"]):
                        raise NoticeError("supplemental license checksum mismatch: " + filename)
                    output.extend(["Upstream file: " + url, "File SHA256: " + entry["sha256"],
                                   "Revision evidence: " + supplement["commit_evidence"], "", data.decode("utf-8")])
            if package["name"] == "libsqlite3-sys":
                text = read("sqlite3/sqlite3.c", 16 * 1024 * 1024)
                blessing = re.search(r"/\*\n\*\* 2001 September 15\n.*?\*\*    May you share freely, never taking more than you give\.\n\*\*\n", text, re.S)
                if not blessing:
                    raise NoticeError("missing bundled SQLite copyright disclaimer")
                output.extend(["Bundled SQLite copyright disclaimer (sqlite3/sqlite3.c):", "", blessing.group(0) + "*/", ""])
    if used != set(supplements):
        raise NoticeError("unused supplemental notice provenance")
    return "\n".join(output).encode("utf-8") + b"\n"


def main(argv=None):
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("--repo", type=Path, default=Path(__file__).resolve().parent.parent)
    parser.add_argument("--cache", type=Path, help="Cargo registry cache directory")
    parser.add_argument("--check", action="store_true", help="refuse if tracked notices differ")
    args = parser.parse_args(argv)
    try:
        result = generate(args.repo, args.cache)
        destination = args.repo / "THIRD-PARTY-NOTICES"
        if args.check:
            if destination.read_bytes() != result:
                raise NoticeError("THIRD-PARTY-NOTICES is stale; regenerate it")
        else:
            destination.write_bytes(result)
    except (NoticeError, OSError, UnicodeError, tarfile.TarError) as error:
        print("notice generation failed: " + str(error), file=sys.stderr)
        return 1
    print("checked" if args.check else "generated", destination)
    return 0


if __name__ == "__main__":
    sys.exit(main())
