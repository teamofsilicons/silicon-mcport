#!/usr/bin/env python3
"""Validate an MCPort backend candidate; install only with explicit --apply.

No credentials are printed, generated or changed. Existing protected runtime
configuration and data are copied opaquely into a root-only stopped-service backup.
"""
from __future__ import annotations
import argparse
import hashlib
import io
import json
import os
from pathlib import Path, PurePosixPath
import platform
import re
import shutil
import stat
import struct
import subprocess
import sys
import tarfile
import tempfile
import time
import urllib.request

MAX_ARCHIVE = 512 * 1024 * 1024
MAX_EXPANDED = 1024 * 1024 * 1024
FIXED_FILES = {"mcport-server", "mcport.service", "Caddyfile.example", "install.py", "README.md", "LICENSE", "BUILD.json", "SHA256SUMS"}
PREFIX = Path("/opt/mcport")
DATA = Path("/var/lib/mcport")
RUNTIME = Path("/etc/mcport/runtime.env")
UNIT = Path("/etc/systemd/system/mcport.service")
BACKUPS = Path("/var/backups/mcport")


def allowed_name(name):
    path = PurePosixPath(name)
    return (name == str(path) and not path.is_absolute() and ".." not in path.parts
            and "\\" not in name and not any(ord(c) < 32 for c in name)
            and (name in FIXED_FILES or (name.startswith("web/dist/")
                 and all(not part.startswith(".") for part in path.parts)
                 and path.suffix.lower() in {".html", ".js", ".css", ".svg", ".txt", ".woff", ".woff2", ".png", ".jpg", ".jpeg", ".webp", ".ico", ".map", ".json"})))


def validate_elf(data):
    if len(data) < 1024 or data[:7] != b"\x7fELF\x02\x01\x01":
        raise ValueError("Backend must be a complete ELF64 little-endian executable")
    kind, machine = struct.unpack_from("<HH", data, 16)
    if kind not in (2, 3) or machine != 183 or struct.unpack_from("<H", data, 52)[0] != 64:
        raise ValueError("Backend must be Linux ARM64")


def validate_bundle(path, expected_digest, expected_revision):
    if not re.fullmatch(r"[0-9a-f]{64}", expected_digest) or not re.fullmatch(r"[0-9a-f]{40}", expected_revision):
        raise ValueError("Expected lowercase SHA-256 and full Git revision")
    path = Path(path)
    if path.is_symlink() or not path.is_file() or path.stat().st_size > MAX_ARCHIVE:
        raise ValueError("Bundle must be a regular file of at most 512 MiB")
    raw = path.read_bytes()
    if hashlib.sha256(raw).hexdigest() != expected_digest:
        raise ValueError("Archive SHA-256 mismatch")
    members = {}
    total = 0
    with tarfile.open(fileobj=io.BytesIO(raw), mode="r:gz") as archive:
        for member in archive:
            total += member.size
            if len(members) >= 5000 or total > MAX_EXPANDED:
                raise ValueError("Archive inventory or size limit exceeded")
            expected_mode = 0o755 if member.name in {"mcport-server", "install.py"} else 0o644
            if (member.size < 0 or not member.isfile() or not allowed_name(member.name) or member.name in members
                    or member.mode != expected_mode or member.uid != 0 or member.gid != 0):
                raise ValueError("Unexpected, duplicate or unsafe archive member")
            members[member.name] = (archive.extractfile(member).read(), member.mode)
    if not FIXED_FILES <= members.keys() or "web/dist/index.html" not in members:
        raise ValueError("Bundle is missing required service, website or provenance files")
    metadata = json.loads(members["BUILD.json"][0])
    if (metadata.get("format_version") != 1 or metadata.get("source_revision") != expected_revision
            or metadata.get("target") != "aarch64-unknown-linux-gnu"
            or metadata.get("build_distribution") != "Amazon Linux 2023"
            or metadata.get("glibc_baseline") != "2.34"
            or metadata.get("native_tests_passed") is not True
            or metadata.get("native_health_smoke_passed") is not True):
        raise ValueError("Unexpected build provenance")
    if not re.fullmatch(r"[0-9]+\.[0-9]+\.[0-9]+(?:[-+][A-Za-z0-9.-]+)?", metadata.get("version", "")):
        raise ValueError("Missing application version")
    if metadata.get("health_version") != metadata["version"] or metadata.get("health_source_revision") != expected_revision:
        raise ValueError("Native health smoke version/revision does not match the package")
    expected = {}
    for line in members["SHA256SUMS"][0].decode("ascii").splitlines():
        if not re.fullmatch(r"[0-9a-f]{64}  .+", line):
            raise ValueError("Malformed checksum inventory")
        digest, name = line.split("  ", 1)
        if name in expected:
            raise ValueError("Duplicate checksum entry")
        expected[name] = digest
    if expected.keys() != members.keys() - {"SHA256SUMS"}:
        raise ValueError("Checksum inventory must cover every payload file exactly")
    for name, digest in expected.items():
        if hashlib.sha256(members[name][0]).hexdigest() != digest:
            raise ValueError("Payload checksum mismatch: " + name)
    validate_elf(members["mcport-server"][0])
    if metadata.get("binary_sha256") != hashlib.sha256(members["mcport-server"][0]).hexdigest():
        raise ValueError("Build evidence does not describe this executable")
    return metadata, members


def command(*args, check=True):
    return subprocess.run(args, check=check, stdout=subprocess.PIPE, stderr=subprocess.PIPE)


def regular_private(path):
    info = path.lstat()
    if not stat.S_ISREG(info.st_mode) or info.st_uid != 0 or stat.S_IMODE(info.st_mode) & 0o077:
        raise ValueError(str(path) + " must be a root-owned private regular file")


def validate_runtime_locations(path):
    # systemd EnvironmentFile overrides Environment= assignments. Refuse a data
    # path our stopped-state backup does not cover; never display file contents.
    required = {"MCPORT_DATA_DIR": "/var/lib/mcport", "MCPORT_BIND": "127.0.0.1:4380"}
    found = {}
    for line in path.read_text().splitlines():
        key, separator, value = line.strip().partition("=")
        key = key.strip()
        if separator and key in required:
            value = value.strip()
            if len(value) >= 2 and value[0] == value[-1] and value[0] in "\"'":
                value = value[1:-1]
            if key in found or value != required[key]:
                raise ValueError("Runtime " + key + " must match the service's fixed location exactly")
            found[key] = value
    if found != required:
        raise ValueError("Runtime must explicitly set the service's fixed MCPORT_DATA_DIR and MCPORT_BIND")


def validate_state_tree(path):
    if path.is_symlink() or not path.is_dir():
        raise ValueError("State directory must not be a symlink")
    for item in path.rglob("*"):
        if item.is_symlink() or not (item.is_file() or item.is_dir()):
            raise ValueError("State backup refuses symlinks and special files")


def root_directory(path, private=False):
    if path.is_symlink():
        raise ValueError("Deployment directories must not be symlinks")
    created = not path.exists()
    path.mkdir(mode=0o700 if private else 0o755, parents=True, exist_ok=True)
    if created:
        path.chmod(0o700 if private else 0o755)
    mode = stat.S_IMODE(path.stat().st_mode)
    if path.stat().st_uid != 0 or mode & (0o077 if private else 0o022):
        raise ValueError("Deployment directories must be root-owned with appropriate protected permissions")
    if not private and mode & 0o111 != 0o111:
        raise ValueError("Public deployment directories must be traversable by the service; set mode 0755 before installing")


def replace_link(path, target):
    temporary = path.with_name(path.name + ".new")
    if temporary.exists() or temporary.is_symlink():
        raise ValueError("Stale deployment link exists: " + str(temporary))
    temporary.symlink_to(target)
    temporary.replace(path)


def health(url, version, revision):
    class NoRedirect(urllib.request.HTTPRedirectHandler):
        def redirect_request(self, request, fp, code, message, headers, new_url):
            return None
    opener = urllib.request.build_opener(urllib.request.ProxyHandler({}), NoRedirect())
    for _ in range(30):
        try:
            with opener.open(url, timeout=2) as response:
                body = json.loads(response.read(4096))
                if (response.status == 200 and body.get("status") == "ok"
                        and body.get("version") == version and body.get("source_revision") == revision):
                    return
        except (OSError, ValueError):
            pass
        time.sleep(1)
    raise RuntimeError("Expected backend health/version/source revision was not observed")


def apply(metadata, members, public_health_url):
    if os.geteuid() != 0 or platform.system() != "Linux" or platform.machine().lower() not in {"aarch64", "arm64"}:
        raise ValueError("Installation requires root on a native Linux ARM64 host")
    from urllib.parse import urlparse
    parsed = urlparse(public_health_url or "")
    if parsed.scheme != "https" or not parsed.hostname or parsed.username or parsed.password or parsed.query or parsed.fragment or parsed.path != "/health":
        raise ValueError("--public-health-url must be the selected HTTPS origin plus /health")
    import fcntl
    import pwd
    account = pwd.getpwnam("mcport")
    regular_private(RUNTIME)
    validate_runtime_locations(RUNTIME)
    validate_state_tree(DATA)
    if DATA.stat().st_uid != account.pw_uid or stat.S_IMODE(DATA.stat().st_mode) & 0o077:
        raise ValueError("/var/lib/mcport must be owned by mcport with mode 0700")
    for parent in (PREFIX, PREFIX / "releases", BACKUPS):
        root_directory(parent, private=parent == BACKUPS)
    current = PREFIX / "current"
    if current.exists() and not current.is_symlink():
        raise ValueError("Current release must be an installer-managed symlink")
    previous = current.resolve() if current.is_symlink() else None
    if previous and (previous.parent != PREFIX / "releases" or not previous.is_dir()):
        raise ValueError("Current release must be inside /opt/mcport/releases")
    if not previous and any(DATA.iterdir()):
        raise ValueError("A first installation requires an empty data directory")
    if UNIT.is_symlink():
        raise ValueError("Service unit must not be a symlink")
    if command("systemctl", "show", "mcport.service", "--property=DropInPaths", "--value", check=False).stdout.strip():
        raise ValueError("Managed installation refuses systemd drop-ins that could override the checked service/configuration")
    release = PREFIX / "releases" / metadata["source_revision"]
    if release.exists():
        raise ValueError("Immutable release already exists; select another revision")
    with (PREFIX / "install.lock").open("a") as lock:
        fcntl.flock(lock, fcntl.LOCK_EX | fcntl.LOCK_NB)
        install_release(metadata, members, public_health_url, release, previous)


def install_release(metadata, members, public_health_url, release, previous):
    current = PREFIX / "current"
    staging = Path(tempfile.mkdtemp(prefix=".staging-", dir=PREFIX / "releases"))
    try:
        for name, (data, mode) in members.items():
            destination = staging / name
            destination.parent.mkdir(parents=True, exist_ok=True)
            destination.write_bytes(data)
            destination.chmod(mode)
        # Validated payloads contain only public release files. Set directory
        # permissions explicitly so an operator's private umask cannot hide
        # website assets from the unprivileged service account.
        for directory in staging.rglob("*"):
            if directory.is_dir():
                directory.chmod(0o755)
        staging.chmod(0o755)
        staging.rename(release)
    finally:
        if staging.exists():
            shutil.rmtree(staging)
    # Check actual loader dependencies on the selected machine before stopping it.
    linked = command("ldd", str(release / "mcport-server"))
    if b"not found" in linked.stdout + linked.stderr:
        raise ValueError("Backend has unresolved native dependencies")
    service_state = command("systemctl", "show", "mcport.service", "--property=ActiveState", "--value", check=False).stdout.decode().strip()
    was_active = service_state in {"active", "activating", "reloading"}
    backup = BACKUPS / (time.strftime("%Y%m%dT%H%M%SZ", time.gmtime()) + "-" + metadata["source_revision"])
    backup.mkdir(mode=0o700)
    if UNIT.exists() or service_state not in {"", "inactive", "failed"}:
        command("systemctl", "stop", "mcport.service")
    service_state = command("systemctl", "show", "mcport.service", "--property=ActiveState", "--value", check=False).stdout.decode().strip()
    if service_state not in {"", "inactive", "failed"}:
        raise RuntimeError("Backend is still running; no state backup or switch performed")
    switched = False
    try:
        validate_state_tree(DATA)
        shutil.copytree(DATA, backup / "data")
        shutil.copy2(RUNTIME, backup / "runtime.env")
        (backup / "runtime.env").chmod(0o600)
        if UNIT.exists():
            shutil.copy2(UNIT, backup / "mcport.service")
        (backup / "receipt.json").write_text(json.dumps({"previous_release": str(previous) if previous else None, "candidate_revision": metadata["source_revision"], "was_active": was_active}, indent=2) + "\n")
        replace_link(current, release)
        switched = True
        unit_temp = UNIT.with_name("mcport.service.new")
        with unit_temp.open("xb") as output:
            output.write(members["mcport.service"][0])
        unit_temp.chmod(0o644)
        unit_temp.replace(UNIT)
        command("systemctl", "daemon-reload")
        if command("systemctl", "show", "mcport.service", "--property=DropInPaths", "--value").stdout.strip():
            raise RuntimeError("Unexpected systemd drop-ins after reload")
        if command("systemctl", "show", "mcport.service", "--property=FragmentPath", "--value").stdout.decode().strip() != str(UNIT):
            raise RuntimeError("The effective systemd unit is not the installed unit")
        command("systemctl", "start", "mcport.service")
        health("http://127.0.0.1:4380/health", metadata["version"], metadata["source_revision"])
        health(public_health_url, metadata["version"], metadata["source_revision"])
        command("systemctl", "enable", "mcport.service")
        print(json.dumps({"installed_revision": metadata["source_revision"], "backup": str(backup), "health": "private_and_public_passed"}))
    except Exception:
        if switched:
            command("systemctl", "stop", "mcport.service", check=False)
            state = command("systemctl", "show", "mcport.service", "--property=ActiveState", "--value", check=False).stdout.decode().strip()
            if state not in {"inactive", "failed"}:
                raise RuntimeError("Cutover failed and service stop could not be confirmed. Candidate link/unit retained. Do not restore data while a writer may be running; inspect systemd and backup " + str(backup))
            if previous:
                replace_link(current, previous)
            elif current.is_symlink():
                current.unlink()
            if (backup / "mcport.service").exists():
                shutil.copy2(backup / "mcport.service", UNIT)
            elif UNIT.exists():
                UNIT.unlink()
            command("systemctl", "daemon-reload", check=False)
            print("Cutover failed. Previous release/unit restored; service remains stopped. Review the protected backup and restore matching data before restarting: " + str(backup), file=sys.stderr)
        elif was_active:
            command("systemctl", "start", "mcport.service", check=False)
        raise


def main():
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("--bundle", type=Path, required=True)
    parser.add_argument("--sha256", required=True)
    parser.add_argument("--revision", required=True)
    parser.add_argument("--apply", action="store_true")
    parser.add_argument("--public-health-url")
    args = parser.parse_args()
    metadata, members = validate_bundle(args.bundle, args.sha256, args.revision)
    if args.apply:
        apply(metadata, members, args.public_health_url)
    else:
        print(json.dumps({"valid": True, "apply": False, "source_revision": metadata["source_revision"], "version": metadata["version"], "target": metadata["target"], "files": len(members), "host_prerequisites": "checked only with --apply"}))


if __name__ == "__main__":
    try:
        main()
    except (OSError, ValueError, RuntimeError, KeyError, subprocess.CalledProcessError) as error:
        print("Backend candidate: " + str(error), file=sys.stderr)
        sys.exit(1)
