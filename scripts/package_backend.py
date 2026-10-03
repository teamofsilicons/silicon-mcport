#!/usr/bin/env python3
"""Build a deterministic deployment candidate from a tested AL2023 ARM64 binary."""
import argparse
import gzip
import hashlib
import importlib.util
import io
import json
import os
from pathlib import Path
import re
import tarfile
import tomllib

ROOT = Path(__file__).resolve().parents[1]
spec = importlib.util.spec_from_file_location("mcport_backend_install", ROOT / "deploy/install.py")
installer = importlib.util.module_from_spec(spec)
spec.loader.exec_module(installer)


def package(binary, web, provenance, revision, output):
    if not re.fullmatch(r"[0-9a-f]{40}", revision):
        raise ValueError("A full lowercase source revision is required")
    binary, web, provenance, output = map(Path, (binary, web, provenance, output))
    if output.suffixes[-2:] != [".tar", ".gz"]:
        raise ValueError("Backend candidates use .tar.gz")
    if binary.is_symlink() or not binary.is_file() or (os.name != "nt" and not binary.stat().st_mode & 0o111):
        raise ValueError("Expected an executable regular backend binary")
    if binary.stat().st_size > installer.MAX_ARCHIVE:
        raise ValueError("Backend binary exceeds size limit")
    data = binary.read_bytes()
    installer.validate_elf(data)
    build = json.loads(provenance.read_text())
    version = tomllib.loads((ROOT / "Cargo.toml").read_text())["workspace"]["package"]["version"]
    if (build.get("source_revision") != revision or build.get("target") != "aarch64-unknown-linux-gnu"
            or build.get("build_distribution") != "Amazon Linux 2023" or build.get("glibc_baseline") != "2.34"
            or build.get("native_tests_passed") is not True or build.get("native_health_smoke_passed") is not True
            or build.get("health_version") != version
            or build.get("health_source_revision") != revision
            or build.get("binary_sha256") != hashlib.sha256(data).hexdigest()
            or not isinstance(build.get("rustc"), str) or not build["rustc"].startswith("rustc ")
            or not re.fullmatch(r"public\.ecr\.aws/amazonlinux/amazonlinux@sha256:[0-9a-f]{64}", build.get("build_image", ""))):
        raise ValueError("Missing or mismatched native build/test evidence")
    build.update(format_version=1, version=version)
    members = {"mcport-server": (data, 0o755)}
    for name, source in {"mcport.service": "deploy/mcport.service", "Caddyfile.example": "deploy/Caddyfile.example", "install.py": "deploy/install.py", "README.md": "deploy/README.md", "LICENSE": "LICENSE"}.items():
        members[name] = ((ROOT / source).read_bytes(), 0o755 if name == "install.py" else 0o644)
    if web.is_symlink() or not web.is_dir():
        raise ValueError("Website build must be a real directory")
    for source in sorted(web.rglob("*")):
        if source.is_symlink():
            raise ValueError("Website symlinks are not allowed")
        if source.is_dir():
            continue
        name = "web/dist/" + source.relative_to(web).as_posix()
        if not source.is_file() or not installer.allowed_name(name):
            raise ValueError("Unexpected website build member: " + name)
        if source.stat().st_size > 32 * 1024 * 1024:
            raise ValueError("Website member exceeds size limit")
        members[name] = (source.read_bytes(), 0o644)
    members["BUILD.json"] = ((json.dumps(build, sort_keys=True, indent=2) + "\n").encode(), 0o644)
    members["SHA256SUMS"] = (("".join(hashlib.sha256(data).hexdigest() + "  " + name + "\n" for name, (data, _) in sorted(members.items()))).encode(), 0o644)
    if "web/dist/index.html" not in members or sum(len(data) for data, _ in members.values()) > installer.MAX_EXPANDED:
        raise ValueError("Missing website index or oversized bundle")
    output.parent.mkdir(parents=True, exist_ok=True)
    with output.open("xb") as destination:
        with gzip.GzipFile(filename="", fileobj=destination, mode="wb", mtime=0, compresslevel=9) as compressed:
            with tarfile.open(fileobj=compressed, mode="w", format=tarfile.USTAR_FORMAT) as archive:
                for name, (data, mode) in sorted(members.items()):
                    entry = tarfile.TarInfo(name)
                    entry.size, entry.mode = len(data), mode
                    archive.addfile(entry, io.BytesIO(data))
    digest = hashlib.sha256(output.read_bytes()).hexdigest()
    installer.validate_bundle(output, digest, revision)
    checksum = output.with_name(output.name + ".sha256")
    with checksum.open("x") as handle:
        handle.write(digest + "  " + output.name + "\n")
    return {"archive": str(output), "sha256": digest, "source_revision": revision, "files": len(members)}


def main():
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("--binary", required=True, type=Path)
    parser.add_argument("--web", required=True, type=Path)
    parser.add_argument("--provenance", required=True, type=Path)
    parser.add_argument("--revision", required=True)
    parser.add_argument("--output", required=True, type=Path)
    args = parser.parse_args()
    print(json.dumps(package(args.binary, args.web, args.provenance, args.revision, args.output)))


if __name__ == "__main__":
    main()
