#!/usr/bin/env python3
"""Validate real native CLI artifacts and assemble a complete Honeycomb candidate.

stage runs a produced CLI on its native host. assemble accepts only one validated
stage archive per declared target. Neither command publishes or creates binaries.
"""
from __future__ import annotations
import argparse
import hashlib
import gzip
import io
import json
import os
from pathlib import Path, PurePosixPath
import platform
import stat
import struct
import subprocess
import sys
import tarfile
import tomllib
import zipfile

import yaml

ROOT = Path(__file__).resolve().parents[1]
TARGETS = {
    "linux-x86_64": ("Linux", "x86_64", "x86_64-unknown-linux-gnu"),
    "linux-aarch64": ("Linux", "aarch64", "aarch64-unknown-linux-gnu"),
    "macos-x86_64": ("Darwin", "x86_64", "x86_64-apple-darwin"),
    "macos-aarch64": ("Darwin", "aarch64", "aarch64-apple-darwin"),
    "windows-x86_64": ("Windows", "x86_64", "x86_64-pc-windows-msvc"),
    "windows-aarch64": ("Windows", "aarch64", "aarch64-pc-windows-msvc"),
}
MAX_BINARY = 512 * 1024 * 1024
FIXED_TIME = (1980, 1, 1, 0, 0, 0)


class PackageError(ValueError):
    pass


class UniqueLoader(yaml.SafeLoader):
    pass


def unique_mapping(loader, node, deep=False):
    result = {}
    for key_node, value_node in node.value:
        key = loader.construct_object(key_node, deep=deep)
        if key in result:
            raise PackageError(f"Duplicate manifest key: {key}")
        result[key] = loader.construct_object(value_node, deep=deep)
    return result


UniqueLoader.add_constructor(yaml.resolver.BaseResolver.DEFAULT_MAPPING_TAG, unique_mapping)


def manifest(path: Path):
    raw = path.read_bytes()
    data = yaml.load(raw, Loader=UniqueLoader)
    if not isinstance(data, dict) or data.get("format_version") != 1 or data.get("app_id") != "mcport":
        raise PackageError("Expected MCPort Honeycomb manifest format_version 1")
    if data.get("bin") != {"mcport": "main"} or set(data.get("targets", {})) != set(TARGETS):
        raise PackageError("Manifest must declare exactly the six native targets and the mcport main executable")
    version = tomllib.loads((ROOT / "Cargo.toml").read_text())["workspace"]["package"]["version"]
    if data.get("version") != version:
        raise PackageError("Honeycomb version must match workspace Cargo version")
    for target, config in data["targets"].items():
        expected = "mcport.exe" if target.startswith("windows-") else "mcport"
        if config != {"root": f"targets/{target}", "executables": {"main": expected}}:
            raise PackageError(f"Unexpected target executable mapping: {target}")
    return data, raw


def binary_path(data, target):
    entry = data["targets"][target]
    return str(PurePosixPath(entry["root"]) / entry["executables"]["main"])


def validate_binary(data: bytes, target: str, mode: int):
    if target not in TARGETS:
        raise PackageError(f"Unknown target: {target}")
    if not 1024 <= len(data) <= MAX_BINARY:
        raise PackageError(f"{target}: empty, truncated or oversized executable")
    if not target.startswith("windows-") and not mode & 0o111:
        raise PackageError(f"{target}: executable permission missing")
    arm = target.endswith("aarch64")
    if target.startswith("linux-"):
        if data[:7] != b"\x7fELF\x02\x01\x01":
            raise PackageError(f"{target}: expected ELF64 little-endian executable")
        file_type, machine = struct.unpack_from("<HH", data, 16)
        if file_type not in (2, 3) or machine != (183 if arm else 62):
            raise PackageError(f"{target}: wrong ELF architecture or executable type")
        if struct.unpack_from("<H", data, 52)[0] != 64:
            raise PackageError(f"{target}: malformed ELF64 header")
    elif target.startswith("macos-"):
        if data[:4] != b"\xcf\xfa\xed\xfe":
            raise PackageError(f"{target}: expected thin Mach-O64 little-endian executable")
        cpu, _, file_type = struct.unpack_from("<III", data, 4)
        if cpu != (0x0100000C if arm else 0x01000007) or file_type != 2:
            raise PackageError(f"{target}: wrong Mach-O architecture or executable type")
    else:
        if data[:2] != b"MZ":
            raise PackageError(f"{target}: expected Windows PE executable")
        offset = struct.unpack_from("<I", data, 0x3C)[0]
        if offset < 64 or offset + 26 > len(data) or data[offset:offset + 4] != b"PE\0\0":
            raise PackageError(f"{target}: malformed PE header")
        machine = struct.unpack_from("<H", data, offset + 4)[0]
        characteristics = struct.unpack_from("<H", data, offset + 22)[0]
        magic = struct.unpack_from("<H", data, offset + 24)[0]
        if machine != (0xAA64 if arm else 0x8664) or magic != 0x20B or not characteristics & 2 or characteristics & 0x2000:
            raise PackageError(f"{target}: wrong PE architecture or not a PE32+ executable")


def read_binary(path: Path, target: str):
    if path.is_symlink() or not path.is_file():
        raise PackageError(f"{target}: produced regular executable is missing: {path}")
    if path.stat().st_size > MAX_BINARY:
        raise PackageError(f"{target}: binary exceeds limit")
    data = path.read_bytes()
    validate_binary(data, target, path.stat().st_mode)
    return data


def write_zip(output: Path, entries: dict[str, tuple[bytes, int]]):
    output.parent.mkdir(parents=True, exist_ok=True)
    # Exclusive creation preserves existing candidates and avoids partial overwrites.
    try:
        with output.open("xb") as destination:
            with zipfile.ZipFile(destination, "w", compression=zipfile.ZIP_STORED, allowZip64=True) as archive:
                for name, (data, mode) in sorted(entries.items()):
                    info = zipfile.ZipInfo(name, FIXED_TIME)
                    info.create_system = 3
                    info.external_attr = (stat.S_IFREG | mode) << 16
                    archive.writestr(info, data)
    except FileExistsError:
        raise PackageError(f"Output already exists: {output}") from None
    except Exception:
        output.unlink(missing_ok=True)
        raise


def write_honeycomb_tar(output: Path, entries: dict[str, tuple[bytes, int]]):
    if not output.name.endswith(".tar.gz"):
        raise PackageError("The complete Honeycomb candidate must end in .tar.gz")
    if sum(len(data) for data, _ in entries.values()) > 2 * 1024 * 1024 * 1024:
        raise PackageError("Expanded Honeycomb payload exceeds 2 GiB")
    output.parent.mkdir(parents=True, exist_ok=True)
    try:
        with output.open("xb") as destination:
            with gzip.GzipFile(filename="", mode="wb", fileobj=destination, mtime=0, compresslevel=9) as compressed:
                with tarfile.open(fileobj=compressed, mode="w", format=tarfile.USTAR_FORMAT) as archive:
                    for name, (data, mode) in sorted(entries.items()):
                        if name != "honeycomb.yaml" and not name.startswith("targets/"):
                            raise PackageError("Honeycomb forbids extra top-level archive entries")
                        info = tarfile.TarInfo(name)
                        info.size, info.mode = len(data), mode
                        info.uid = info.gid = info.mtime = 0
                        info.uname = info.gname = ""
                        archive.addfile(info, io.BytesIO(data))
        if output.stat().st_size > 512 * 1024 * 1024:
            raise PackageError("Compressed Honeycomb archive exceeds 512 MiB")
    except FileExistsError:
        raise PackageError(f"Output already exists: {output}") from None
    except Exception:
        output.unlink(missing_ok=True)
        raise


def checksum(path: Path):
    with path.open("rb") as source:
        digest = hashlib.file_digest(source, "sha256").hexdigest()
    line = f"{digest}  {path.name}\n"
    path.with_suffix(path.suffix + ".sha256").write_text(line)
    return digest


def stage(args):
    data, _ = manifest(args.manifest)
    payload = read_binary(args.binary, args.target)
    system, arch, triple = TARGETS[args.target]
    host_arch = {"AMD64": "x86_64", "arm64": "aarch64", "ARM64": "aarch64"}.get(platform.machine(), platform.machine())
    if platform.system() != system or host_arch != arch:
        raise PackageError(f"Stage {args.target} on its native runner; this host is {platform.system()} {host_arch}")
    completed = subprocess.run([str(args.binary.resolve()), "--version"], check=True, capture_output=True, text=True, timeout=15)
    if completed.stdout.strip() != f"mcport {data['version']}":
        raise PackageError("Produced CLI --version does not match the release manifest")
    record = {"target": args.target, "rust_target": triple, "version": data["version"], "sha256": hashlib.sha256(payload).hexdigest(), "native_smoke": True}
    write_zip(args.output, {binary_path(data, args.target): (payload, 0o755), f"build/{args.target}.json": (json.dumps(record, sort_keys=True, separators=(",", ":")).encode(), 0o644)})
    print(json.dumps({"target": args.target, "archive": str(args.output), "sha256": checksum(args.output)}))


def assemble(args):
    data, raw = manifest(args.manifest)
    files = sorted(args.input.glob("mcport-*.zip"))
    seen = set()
    entries = {"honeycomb.yaml": (raw, 0o644)}
    provenance = {}
    for path in files:
        with zipfile.ZipFile(path) as archive:
            infos = archive.infolist()
            metadata = [info for info in infos if info.filename.startswith("build/") and info.filename.endswith(".json")]
            if len(infos) != 2 or len(metadata) != 1 or metadata[0].file_size > 4096:
                raise PackageError(f"Unexpected files in native stage archive: {path}")
            record = json.loads(archive.read(metadata[0]))
            target = record.get("target")
            if target not in TARGETS or target in seen or metadata[0].filename != f"build/{target}.json":
                raise PackageError(f"Duplicate or unknown target in {path}")
            expected_path = binary_path(data, target)
            if {info.filename for info in infos} != {metadata[0].filename, expected_path}:
                raise PackageError(f"Unexpected native archive paths in {path}")
            info = archive.getinfo(expected_path)
            mode = info.external_attr >> 16
            if not stat.S_ISREG(mode) or info.file_size > MAX_BINARY:
                raise PackageError(f"Expected bounded regular executable in {path}")
            payload = archive.read(info)
            validate_binary(payload, target, mode)
            if record != {"target": target, "rust_target": TARGETS[target][2], "version": data["version"], "sha256": hashlib.sha256(payload).hexdigest(), "native_smoke": True}:
                raise PackageError(f"Native artifact provenance/version/checksum mismatch in {path}")
            entries[expected_path] = (payload, 0o755)
            provenance[target] = record
            seen.add(target)
    if seen != set(TARGETS):
        raise PackageError("Missing native targets: " + ", ".join(sorted(set(TARGETS) - seen)))
    write_honeycomb_tar(args.output, entries)
    args.output.with_name(args.output.name + ".provenance.json").write_text(json.dumps(provenance, indent=2, sort_keys=True) + "\n")
    digest = checksum(args.output)
    sums = args.output.parent / "SHA256SUMS"
    sums.write_text(f"{digest}  {args.output.name}\n")
    print(json.dumps({"archive": str(args.output), "targets": sorted(seen), "sha256": digest}))


def main():
    parser = argparse.ArgumentParser(description=__doc__)
    sub = parser.add_subparsers(dest="operation", required=True)
    one = sub.add_parser("stage", help="Validate and archive an actual native CLI")
    one.add_argument("--manifest", type=Path, default=ROOT / "honeycomb.yaml")
    one.add_argument("--target", choices=TARGETS, required=True)
    one.add_argument("--binary", type=Path, required=True)
    one.add_argument("--output", type=Path, required=True)
    all_targets = sub.add_parser("assemble", help="Require all six native stage archives and assemble a Honeycomb .tar.gz candidate")
    all_targets.add_argument("--manifest", type=Path, default=ROOT / "honeycomb.yaml")
    all_targets.add_argument("--input", type=Path, required=True)
    all_targets.add_argument("--output", type=Path, required=True)
    args = parser.parse_args()
    try:
        (stage if args.operation == "stage" else assemble)(args)
    except (PackageError, OSError, ValueError, KeyError, yaml.YAMLError, zipfile.BadZipFile, subprocess.SubprocessError) as error:
        parser.exit(1, f"Packaging failed: {error}\n")


if __name__ == "__main__":
    main()
