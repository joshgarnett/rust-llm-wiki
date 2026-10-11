#!/usr/bin/env python3
"""One pinned real-editor attempt on a fresh GitHub-hosted macOS or Ubuntu VM."""
import argparse
import errno
import hashlib
import json
import os
from pathlib import Path
import plistlib
import re
import signal
import shutil
import socket
import subprocess
import sys
import time
import urllib.request

VERSION = "1.10.6"
SIZE = 210410377
SHA256 = "a7fbd87353d7802933a13e3d90f55c39114c419ac48d459a356bdab73c128a8a"
URL = "https://github.com/obsidianmd/obsidian-releases/releases/download/v1.10.6/Obsidian-1.10.6.dmg"
LINUX_SIZE = 86313124
LINUX_SHA256 = "7e2fcf3fa6da54715d88a17ac906c87ca397c9d1e15ac6a0f6ba74f097a749d6"
LINUX_URL = "https://github.com/obsidianmd/obsidian-releases/releases/download/v1.10.6/obsidian_1.10.6_amd64.deb"
PROFILE_SHA256 = "f7be92346c99b636f3877c466ddb114c148ef90f5225da24d67a62d3906e2d13"


def command(evidence, env, name, argv, timeout=30):
    before = time.monotonic()
    timed_out = False
    try:
        p = subprocess.run(list(map(str, argv)), env=env, capture_output=True, timeout=timeout)
        stdout, stderr, code = p.stdout, p.stderr, p.returncode
    except subprocess.TimeoutExpired as error:
        stdout, stderr, code = error.stdout or b"", error.stderr or b"", None
        timed_out = True
    (evidence / (name + ".stdout")).write_bytes(stdout)
    (evidence / (name + ".stderr")).write_bytes(stderr)
    with (evidence / "preparation.jsonl").open("a") as f:
        f.write(json.dumps({"name": name, "argv": list(map(str, argv)), "exit_code": code, "timed_out": timed_out, "elapsed_seconds": time.monotonic() - before}) + "\n")
    if timed_out or code:
        raise RuntimeError(name + " failed; inspect retained stderr")
    return stdout


def linux_session(argv):
    """Run only after unshare/setpriv; never install or change host security here."""
    if sys.platform != "linux" or os.environ.get("GITHUB_ACTIONS") != "true" or os.environ.get("RUNNER_ENVIRONMENT") != "github-hosted":
        raise RuntimeError("Linux session requires a fresh GitHub-hosted Linux VM")
    node, controller, binary, directory, binary_hash, uid, gid, parent_namespace = argv
    evidence = Path(directory)
    env = dict(os.environ)
    uid, gid = int(uid), int(gid)
    status = {"status": "running", "uid": os.getuid(), "euid": os.geteuid(), "gid": os.getgid(), "egid": os.getegid(),
              "network_namespace": os.readlink("/proc/self/ns/net"), "parent_network_namespace": parent_namespace}
    display = None
    try:
        if uid <= 0 or gid <= 0 or (os.getuid(), os.geteuid(), os.getgid(), os.getegid()) != (uid, uid, gid, gid):
            raise RuntimeError("namespace wrapper did not drop to the original runner credentials")
        if os.getpgrp() != os.getpid():
            os.setsid()
        save(evidence / "namespace-session-pid.json", {"pid": os.getpid(), "uid": uid, "gid": gid, "network_namespace": status["network_namespace"]})
        if status["network_namespace"] == parent_namespace:
            raise RuntimeError("namespace wrapper retained the host network namespace")
        proc_status = Path("/proc/self/status").read_text().splitlines()
        status["capabilities"] = {row.split(":")[0]: row.split(":")[1].strip() for row in proc_status if row.startswith(("CapEff:", "CapPrm:", "CapAmb:"))}
        if any(int(value, 16) for value in status["capabilities"].values()):
            raise RuntimeError("nonroot namespace session retained effective/permitted/ambient capabilities")
        status["interfaces"] = json.loads(command(evidence, env, "namespace-interfaces", ["/usr/sbin/ip", "-j", "address"]))
        status["routes_v4"] = json.loads(command(evidence, env, "namespace-routes-v4", ["/usr/sbin/ip", "-j", "-4", "route", "show", "table", "all"]))
        status["routes_v6"] = json.loads(command(evidence, env, "namespace-routes-v6", ["/usr/sbin/ip", "-j", "-6", "route", "show", "table", "all"]))
        if [row["ifname"] for row in status["interfaces"]] != ["lo"] or status["routes_v4"] or status["routes_v6"]:
            raise RuntimeError("private network namespace has an unexpected interface or route")
        status["network_probes"] = {}
        for name, family, endpoint in [("ipv4", socket.AF_INET, ("192.0.2.1", 9)), ("ipv6", socket.AF_INET6, ("2001:db8::1", 9))]:
            with socket.socket(family) as sock:
                sock.settimeout(1)
                try:
                    sock.connect(endpoint)
                    status["network_probes"][name] = "allowed"
                except OSError as error:
                    status["network_probes"][name] = {"errno": error.errno, "error": str(error)}
            probe_errno = status["network_probes"][name].get("errno") if isinstance(status["network_probes"][name], dict) else None
            no_ipv6_address = not any(address.get("family") == "inet6" for interface in status["interfaces"] for address in interface.get("addr_info", []))
            denied = probe_errno == errno.ENETUNREACH or (name == "ipv6" and probe_errno == errno.EADDRNOTAVAIL and no_ipv6_address)
            if not denied:
                raise RuntimeError("private namespace did not reject " + name + " routing with the required no-route/no-address error")
        endpoint = str(evidence / "namespace-probe.sock")
        try:
            with socket.socket(socket.AF_UNIX) as server, socket.socket(socket.AF_UNIX) as client:
                server.bind(endpoint); server.listen(); client.connect(endpoint)
                with server.accept()[0] as peer:
                    client.sendall(b"owned"); status["owned_unix_allowed"] = peer.recv(5) == b"owned"
        finally:
            Path(endpoint).unlink(missing_ok=True)
        if not status["owned_unix_allowed"]:
            raise RuntimeError("owned Unix socket preflight failed")
        status["status"] = "passed"
        save(evidence / "network-preflight.json", status)
        # Authentication is private to this fixture; the cookie never enters evidence logs.
        import secrets
        number = next(n for n in range(91, 111) if not Path("/tmp/.X%d-lock" % n).exists() and not Path("/tmp/.X11-unix/X%d" % n).exists())
        authority = evidence / "display.Xauthority"
        authority.touch(mode=0o600)
        auth = subprocess.run(["/usr/bin/xauth", "-f", str(authority), "add", ":" + str(number), "MIT-MAGIC-COOKIE-1", secrets.token_hex(16)], env=env, capture_output=True, timeout=10)
        (evidence / "xauth.stdout").write_bytes(auth.stdout)
        (evidence / "xauth.stderr").write_bytes(auth.stderr)
        save(evidence / "xauth.json", {"argv": ["/usr/bin/xauth", "-f", str(authority), "add", ":" + str(number), "MIT-MAGIC-COOKIE-1", "<owned-cookie>"], "exit_code": auth.returncode, "uid": authority.stat().st_uid, "mode": oct(authority.stat().st_mode & 0o777)})
        if auth.returncode:
            raise RuntimeError("owned X authorization setup failed")
        display_env = dict(env, DISPLAY=":" + str(number), XAUTHORITY=str(authority))
        command(evidence, env, "xvfb-version", ["/usr/bin/dpkg-query", "-W", "-f=${Version}\n", "xvfb"])
        display_argv = ["/usr/bin/Xvfb", ":" + str(number), "-screen", "0", "1280x960x24", "-nolisten", "tcp", "-auth", str(authority)]
        with (evidence / "xvfb.log").open("wb") as log:
            display = subprocess.Popen(display_argv, env=display_env, stdout=log, stderr=log, start_new_session=True)
        save(evidence / "display-pid.json", {"pid": display.pid, "argv": display_argv, "uid": uid, "gid": gid, "display": display_env["DISPLAY"], "network_namespace": status["network_namespace"]})
        ready = time.monotonic() + 10
        while time.monotonic() < ready:
            if display.poll() is not None:
                raise RuntimeError("owned Xvfb exited before display readiness")
            probe = subprocess.run(["/usr/bin/xdpyinfo", "-display", display_env["DISPLAY"]], env=display_env, capture_output=True, timeout=2)
            if probe.returncode == 0:
                (evidence / "display-ready.stdout").write_bytes(probe.stdout)
                break
            time.sleep(0.1)
        else:
            (evidence / "display-ready.stdout").write_bytes(probe.stdout)
            (evidence / "display-ready.stderr").write_bytes(probe.stderr)
            raise RuntimeError("authorized owned Xvfb display did not become ready")
        controller_started = time.monotonic()
        p = subprocess.run([node, controller, binary, str(evidence), binary_hash], env=display_env, timeout=330)
        save(evidence / "linux-controller-exit.json", {"returncode": p.returncode,
             "elapsed_seconds": time.monotonic() - controller_started,
             "app_before_display_teardown": owned_group_state(evidence, "app")})
        return p.returncode
    except Exception as error:
        status["status"] = "failed"; status["error"] = str(error)
        save(evidence / "linux-session-error.json", status)
        return 1
    finally:
        if display:
            diagnostic_error = None
            try:
                save(evidence / "display-before-teardown.json", {"pid": display.pid,
                     "display_returncode": display.poll(),
                     "app": owned_group_state(evidence, "app")})
            except Exception as error:
                diagnostic_error = str(error)
            signals = []
            if group_alive(display.pid):
                signals.append("SIGTERM")
                os.killpg(display.pid, signal.SIGTERM)
                try:
                    display.wait(timeout=2)
                except subprocess.TimeoutExpired:
                    signals.append("SIGKILL")
                    os.killpg(display.pid, signal.SIGKILL); display.wait(timeout=2)
            gone = not group_alive(display.pid)
            save(evidence / "display-cleanup.json", {"owned_process_group_gone": gone,
                 "pid": display.pid, "signals": signals, "returncode": display.returncode})
            if not gone:
                raise RuntimeError("owned Xvfb group survived cleanup")
            if diagnostic_error:
                raise RuntimeError("display teardown diagnostics unavailable: " + diagnostic_error)


def digest(path):
    with Path(path).open("rb") as f:
        return hashlib.file_digest(f, "sha256").hexdigest() if hasattr(hashlib, "file_digest") else hashlib.sha256(f.read()).hexdigest()


def save(path, value):
    path.write_text(json.dumps(value, indent=2) + "\n")


def group_alive(pid):
    try:
        os.killpg(pid, 0)
        return True
    except ProcessLookupError:
        return False


def owned_group_state(evidence, name):
    """Observe only a group PID explicitly created and recorded by this harness."""
    pidfile = evidence / (name + "-pid.json")
    if not pidfile.exists():
        return {"recorded": False}
    pid = json.loads(pidfile.read_text())["pid"]
    if not isinstance(pid, int) or pid <= 1:
        raise RuntimeError("invalid owned process group PID")
    state = {"recorded": True, "pid": pid, "group_alive": group_alive(pid)}
    if sys.platform == "linux":
        try:
            with Path("/proc/%d/status" % pid).open() as status:
                rows = status.read(8192).splitlines()
            state["process_status"] = {row.split(":", 1)[0]: row.split(":", 1)[1].strip()
                 for row in rows if row.split(":", 1)[0] in {"Name", "State", "PPid", "Uid", "Gid", "NSpid"}}
        except OSError as error:
            state["process_status_error"] = {"errno": error.errno, "message": str(error)}
    return state


def cleanup_groups(evidence, info):
    """Signal only process groups explicitly created and recorded by this harness."""
    for name in ["app", "display", "namespace-session"]:
        pidfile = evidence / (name + "-pid.json")
        if not pidfile.exists():
            continue
        pid = json.loads(pidfile.read_text())["pid"]
        if not isinstance(pid, int) or pid <= 1:
            raise RuntimeError("invalid owned process group PID")
        observation = {"name": name, **owned_group_state(evidence, name), "signals": []}
        info.setdefault("owned_group_cleanup", []).append(observation)
        if observation["group_alive"]:
            observation["signals"].append("SIGKILL")
            os.killpg(pid, signal.SIGKILL)
            info["status"] = "failed"
            info["cleanup_error"] = "controller left an owned " + name + " process group; supervisor killed it"
            info.setdefault("error", info["cleanup_error"])
            until = time.monotonic() + 2
            while group_alive(pid) and time.monotonic() < until:
                time.sleep(0.1)
        info[name + "_process_group_gone"] = not group_alive(pid)
        if not info[name + "_process_group_gone"]:
            info["status"] = "failed"
            info["cleanup_error"] = "owned " + name + " process group survived supervisor cleanup"
            info.setdefault("error", info["cleanup_error"])
    if "app_process_group_gone" in info:
        info["owned_process_group_gone"] = info["app_process_group_gone"]


def main():
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("--binary", required=True)
    parser.add_argument("--binary-sha256", required=True)
    parser.add_argument("--source-commit", required=True)
    parser.add_argument("--archive", required=True)
    parser.add_argument("--archive-sha256", required=True)
    parser.add_argument("--evidence", required=True)
    args = parser.parse_args()
    # Refuse before creating any fixture or running any executable on a personal host.
    if sys.platform not in {"darwin", "linux"} or os.environ.get("GITHUB_ACTIONS") != "true" or os.environ.get("RUNNER_ENVIRONMENT") != "github-hosted":
        parser.error("real editor qualification requires a fresh GitHub-hosted macOS or Ubuntu VM")
    linux = sys.platform == "linux"
    if linux:
        release = Path("/etc/os-release").read_text()
        if not re.search(r'^ID=ubuntu$', release, re.M) or not re.search(r'^VERSION_ID="24.04"$', release, re.M) or os.uname().machine != "x86_64" or os.getuid() == 0:
            parser.error("Linux qualification requires Ubuntu 24.04 x86_64 and a nonroot runner")
    root = Path(os.environ["RUNNER_TEMP"]).resolve(strict=True)
    evidence = Path(args.evidence).resolve()
    if root not in evidence.parents:
        parser.error("evidence must be a new directory below RUNNER_TEMP")
    binary = Path(args.binary).resolve(strict=True)
    archive = Path(args.archive).resolve(strict=True)
    if not re.fullmatch(r"[0-9a-f]{40}", args.source_commit) or any(not re.fullmatch(r"[0-9a-f]{64}", value) for value in [args.binary_sha256, args.archive_sha256]):
        parser.error("source commit and SHA-256 pins must be full lowercase hashes")
    if digest(binary) != args.binary_sha256:
        parser.error("binary differs from the pinned archive executable")
    if digest(archive) != args.archive_sha256:
        parser.error("archive differs from its pinned downloaded bytes")
    evidence.mkdir(mode=0o700)
    harness = Path(__file__).resolve().parent
    env = {"PATH": "/usr/bin:/bin:/usr/sbin:/sbin", "HOME": str(evidence / "home"),
           "TMPDIR": str(evidence / "temp") + "/", "LC_ALL": "C.UTF-8" if linux else "en_US.UTF-8", "TZ": "UTC",
           "XDG_CONFIG_HOME": str(evidence / "profile"), "XDG_CACHE_HOME": str(evidence / "cache")}
    controller_env = dict(env, GITHUB_ACTIONS="true", RUNNER_ENVIRONMENT="github-hosted")
    for directory in ["home", "temp", "profile", "cache", "mount"]:
        (evidence / directory).mkdir(mode=0o700)
    url, size, download_hash = (LINUX_URL, LINUX_SIZE, LINUX_SHA256) if linux else (URL, SIZE, SHA256)
    info = {"status": "running", "source_commit": args.source_commit,
            "python_version": sys.version, "python_executable": sys.executable,
            "binary_sha256": args.binary_sha256, "obsidian_version": VERSION,
            "archive_sha256": args.archive_sha256,
            "download_url": url, "download_sha256": download_hash, "download_bytes": size,
            "app_network": "private network namespace without external routes" if linux else "allowed by normal macOS app launch",
            "cli_network": "offline", "app_sandbox": "unmodified shipped Obsidian settings",
            "harness_sha256": {p.name: digest(p) for p in [Path(__file__).resolve(), harness / "roundtrip.mjs"]},
            "runner": {key: os.environ.get(key) for key in ["RUNNER_ENVIRONMENT", "RUNNER_OS", "RUNNER_ARCH", "ImageOS", "ImageVersion", "GITHUB_SHA", "GITHUB_RUN_ID", "GITHUB_RUN_ATTEMPT"]}}
    save(evidence / "qualification.json", info)
    mounted = False
    started = time.monotonic()

    def prepare(name, argv, timeout=30):
        return command(evidence, env, name, argv, timeout)

    try:
        if linux:
            (evidence / "os-release").write_text(release)
            info["linux_version"] = prepare("linux-version", ["/usr/bin/uname", "-a"]).decode().strip()
        else:
            info["macos_version"] = prepare("macos-version", ["/usr/bin/sw_vers", "-productVersion"]).decode().strip()
        installer = evidence / ("obsidian_1.10.6_amd64.deb" if linux else "Obsidian-1.10.6.dmg")
        before = time.monotonic()
        count = 0
        with urllib.request.urlopen(url, timeout=30) as response, installer.open("xb") as out:
            if response.geturl().split("/")[2] not in {"github.com", "release-assets.githubusercontent.com", "objects.githubusercontent.com"}:
                raise RuntimeError("download redirected outside official GitHub release assets")
            while True:
                block = response.read(1024 * 1024)
                if not block:
                    break
                count += len(block)
                if count > size or time.monotonic() - before > 180:
                    raise RuntimeError("official app download exceeded pinned size/time bound")
                out.write(block)
        if count != size or digest(installer) != download_hash:
            raise RuntimeError("official app download differs from pinned bytes")
        if linux:
            prepare("package-install", ["/usr/bin/sudo", "-n", "/usr/bin/timeout", "--kill-after=5s", "175s", "/usr/bin/env", "DEBIAN_FRONTEND=noninteractive", "/usr/bin/apt-get", "install", "--yes", "--no-install-recommends", installer, "xvfb", "xauth", "x11-utils"], 185)
            installed = prepare("package-version", ["/usr/bin/dpkg-query", "-W", "-f=${Version} ${Architecture}\n", "obsidian"]).decode().strip()
            if installed != VERSION + " amd64":
                raise RuntimeError("installed package version/architecture differs from pinned version")
            installed = installed.split()[0]
            if prepare("package-verify", ["/usr/bin/dpkg", "--verify", "obsidian"]).strip():
                raise RuntimeError("installed package files differ from package checksums")
            metadata = {}
            for filename in ["/opt/Obsidian/obsidian", "/opt/Obsidian/chrome-sandbox", "/opt/Obsidian/resources/apparmor-profile", "/etc/apparmor.d/obsidian"]:
                target = Path(filename); stat = target.stat()
                metadata[filename] = {"sha256": digest(target), "uid": stat.st_uid, "gid": stat.st_gid, "mode": oct(stat.st_mode & 0o7777), "bytes": stat.st_size}
            for filename in ["/opt/Obsidian/resources/apparmor-profile", "/etc/apparmor.d/obsidian"]:
                if metadata[filename]["sha256"] != PROFILE_SHA256:
                    raise RuntimeError("installed vendor AppArmor profile differs from pinned profile")
            save(evidence / "installed-package-files.json", metadata)
            (evidence / "vendor-apparmor-profile").write_bytes(Path("/opt/Obsidian/resources/apparmor-profile").read_bytes())
            prepare("apparmor-status", ["/usr/bin/sudo", "-n", "/usr/sbin/aa-status"])
            loaded = prepare("apparmor-loaded-profiles", ["/usr/bin/sudo", "-n", "/usr/bin/cat", "/sys/kernel/security/apparmor/profiles"]).decode()
            if not re.search(r"^obsidian \(", loaded, re.M):
                raise RuntimeError("vendor Obsidian AppArmor profile is not loaded")
            prepare("sandbox-settings", ["/usr/bin/cat", "/sys/module/apparmor/parameters/enabled", "/proc/sys/kernel/apparmor_restrict_unprivileged_userns"])
            prepare("namespace-tools-version", ["/usr/bin/unshare", "--version"])
            prepare("privilege-tools-version", ["/usr/bin/setpriv", "--version"])
        else:
            prepare("attach", ["/usr/bin/hdiutil", "attach", "-readonly", "-nobrowse", "-mountpoint", evidence / "mount", installer], 60)
            mounted = True
            app = evidence / "Obsidian.app"
            prepare("copy", ["/usr/bin/ditto", evidence / "mount" / "Obsidian.app", app], 60)
            prepare("detach", ["/usr/bin/hdiutil", "detach", evidence / "mount"], 30)
            mounted = False
            prepare("codesign", ["/usr/bin/codesign", "--verify", "--deep", "--strict", "--verbose=2", app], 30)
            with (app / "Contents" / "Info.plist").open("rb") as f:
                installed = plistlib.load(f)["CFBundleShortVersionString"]
        if installed != VERSION:
            raise RuntimeError("download installer version differs from pinned version")
        # This is a new disposable app setting, not a fabricated vault registry.
        save(evidence / "profile" / "obsidian.json", {"updateDisabled": True})
        node = shutil.which("node")
        if not node:
            raise RuntimeError("Node.js is required for the inherited CDP pipe controller")
        info["node_version"] = prepare("node-version", [node, "--version"]).decode().strip()
        info["node_executable"] = node
        argv = [node, str(harness / "roundtrip.mjs"), str(binary), str(evidence), args.binary_sha256]
        if linux:
            argv = ["/usr/bin/sudo", "-n", "/usr/bin/unshare", "--net", "/usr/bin/setpriv", "--reuid", str(os.getuid()), "--regid", str(os.getgid()), "--init-groups", "/usr/bin/env", "-i", *[key + "=" + value for key, value in controller_env.items()], sys.executable, str(Path(__file__).resolve()), "--linux-session", node, str(harness / "roundtrip.mjs"), str(binary), str(evidence), args.binary_sha256, str(os.getuid()), str(os.getgid()), os.readlink("/proc/self/ns/net")]
        save(evidence / "controller-launch.json", {"argv": argv, "env": controller_env})
        with (evidence / "controller.stdout").open("wb") as out, (evidence / "controller.stderr").open("wb") as err:
            p = subprocess.Popen(argv, env=controller_env, stdout=out, stderr=err)
            controller_started = time.monotonic()
            def record_controller_exit():
                info["controller_exit_code"] = p.returncode
                save(evidence / "outer-controller-exit.json", {"returncode": p.returncode,
                     "elapsed_seconds": time.monotonic() - controller_started})
            app_observed = None
            while p.poll() is None:
                now = time.monotonic()
                if app_observed is None and (evidence / "app-pid.json").exists():
                    app_observed = now
                if now - controller_started > 330 or app_observed is not None and now - app_observed > 295:
                    # Independent watchdog covers a hung or unexpectedly failed controller.
                    cleanup_groups(evidence, info)
                    p.kill()
                    p.wait(timeout=5)
                    record_controller_exit()
                    raise RuntimeError("independent controller/app watchdog expired")
                time.sleep(0.2)
            record_controller_exit()
        if p.returncode:
            raise RuntimeError("real editor roundtrip failed; inspect result.json and controller logs")
        if digest(binary) != args.binary_sha256:
            raise RuntimeError("pinned lwiki executable changed")
        info["status"] = "passed"
    except Exception as error:
        info["status"] = "failed"
        info["error"] = str(error)
    finally:
        try:
            cleanup_groups(evidence, info)
        except Exception as error:
            info["status"] = "failed"; info["cleanup_error"] = str(error)
        if linux and info["status"] != "passed":
            try:
                prepare("kernel-diagnostics", ["/usr/bin/sudo", "-n", "/usr/bin/journalctl", "-k", "--no-pager", "-n", "200"], 10)
            except Exception as error:
                info["kernel_diagnostics_error"] = str(error)
        info["binary_final_sha256"] = digest(binary)
        info["archive_final_sha256"] = digest(archive)
        if info["binary_final_sha256"] != args.binary_sha256:
            info["status"] = "failed"
            info["error"] = "pinned lwiki executable changed"
        if info["archive_final_sha256"] != args.archive_sha256:
            info["status"] = "failed"
            info["error"] = "pinned archive changed"
        if mounted:
            try:
                prepare("detach-finally", ["/usr/bin/hdiutil", "detach", evidence / "mount"], 15)
            except Exception as error:
                info["cleanup_error"] = str(error)
                info["status"] = "failed"
        info["elapsed_seconds"] = time.monotonic() - started
        save(evidence / "qualification.json", info)
    print(json.dumps({"status": info["status"], "evidence": str(evidence)}))
    return 0 if info["status"] == "passed" else 1


if __name__ == "__main__":
    sys.exit(linux_session(sys.argv[2:]) if len(sys.argv) > 1 and sys.argv[1] == "--linux-session" else main())
