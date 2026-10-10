#!/usr/bin/env python3
"""Package one target of the mcport CLI for Silicon Apps.

Run it through scripts/package-apps.sh:

    scripts/package-apps.sh [options] VERSION TARGET BINARY

It stages `apps.yaml` (rendered from packaging/apps.yaml.in, listing only TARGET)
and `bin/mcport` (`bin/mcport.exe` on Windows), runs `silicon-apps validate` and
`silicon-apps pack`, and writes dist/apps/mcport-VERSION-TARGET.tar.gz with a
`.sha256` file. Whenever this machine can run the binary (natively, or through
the emulator in PACKAGE_APPS_EMULATOR), it must first answer the three commands
Silicon Apps checks (`--help`, `accounts --json`, `login status --json`) in an
empty home, and the packed archive is extracted and checked again. A binary that
fails is never packed. `--check-only` runs only the binary checks; CI runs it on
each native runner and hands the record to the packaging job.

Standard library only, so it runs on every release runner without installs.
"""
from __future__ import annotations

import argparse
import errno
import hashlib
import json
import os
from pathlib import Path
import platform
import re
import shlex
import shutil
import struct
import subprocess
import sys
import tarfile
import tempfile
import tomllib

ROOT = Path(__file__).resolve().parents[1]
APP_ID = "mcport"
COMMAND = "mcport"
TEMPLATE = ROOT / "packaging" / "apps.yaml.in"
DEFAULT_OUTPUT = ROOT / "dist" / "apps"
MAX_BINARY = 512 * 1024 * 1024
MIN_SILICON_APPS = (0, 2, 0)
VERSION = re.compile(r"(0|[1-9][0-9]*)\.(0|[1-9][0-9]*)\.(0|[1-9][0-9]*)")
# Silicon Apps target -> (platform.system() of a machine that runs it, architecture)
TARGETS = {
    "linux-x86_64": ("Linux", "x86_64"),
    "linux-i686": ("Linux", "i686"),
    "linux-aarch64": ("Linux", "aarch64"),
    "linux-armv7hf": ("Linux", "armv7hf"),
    "macos-x86_64": ("Darwin", "x86_64"),
    "macos-aarch64": ("Darwin", "aarch64"),
    "windows-x86_64": ("Windows", "x86_64"),
    "windows-i686": ("Windows", "i686"),
    "windows-aarch64": ("Windows", "aarch64"),
}
ELF_MACHINES = {"x86_64": (2, 62, "x86-64"), "i686": (1, 3, "i386"), "aarch64": (2, 183, "AArch64"), "armv7hf": (1, 40, "ARM")}
MACHO_CPUS = {"x86_64": 0x01000007, "aarch64": 0x0100000C}
PE_MACHINES = {"x86_64": (0x8664, 0x20B), "i686": (0x14C, 0x10B), "aarch64": (0xAA64, 0x20B)}
# Exec failures that mean "this machine cannot run that format", not "the binary is broken".
CANNOT_EXECUTE_ERRNOS = {errno.ENOEXEC, 86}  # 86 is EBADARCH ("Bad CPU type in executable") on macOS
CANNOT_EXECUTE_WINERRORS = {193, 216}  # ERROR_BAD_EXE_FORMAT, ERROR_EXE_MACHINE_TYPE_MISMATCH


class PackageError(Exception):
    """A refusal with what failed, why, and what to do; printed as one message."""


class CannotRun(Exception):
    """This machine cannot execute the binary's format (a cross-built target)."""


def workspace_version() -> str:
    return tomllib.loads((ROOT / "Cargo.toml").read_text())["workspace"]["package"]["version"]


def check_version(version: str) -> str:
    if not VERSION.fullmatch(version):
        raise PackageError(f"Version {version!r} is not a plain x.y.z version; Silicon Apps accepts no prerelease or build suffix.")
    expected = workspace_version()
    if version != expected:
        raise PackageError(f"Version {version} does not match the workspace version {expected} in Cargo.toml; package the version the binary was built from.")
    return version


def check_target(target: str) -> str:
    if target == "host":
        return host_target()
    if target not in TARGETS:
        raise PackageError(f"Unknown target {target!r}; use one of: {', '.join(TARGETS)}.")
    return target


def host_target() -> str:
    system = platform.system()
    machine = platform.machine().lower()
    arch = {"amd64": "x86_64", "x64": "x86_64", "arm64": "aarch64", "i386": "i686", "i586": "i686", "x86": "i686"}.get(machine, machine)
    if arch.startswith("armv7"):
        arch = "armv7hf"
    for name, (target_system, target_arch) in TARGETS.items():
        if (target_system, target_arch) == (system, arch):
            return name
    raise PackageError(f"This machine ({system} {platform.machine()}) is not a Silicon Apps target; name the target explicitly.")


def binary_name(target: str) -> str:
    return COMMAND + (".exe" if target.startswith("windows-") else "")


def read_binary(path: Path) -> bytes:
    if path.is_symlink() or not path.is_file():
        raise PackageError(f"{path} is not a regular file; pass the built executable (for example target/<rust-target>/release/{COMMAND}).")
    size = path.stat().st_size
    if size < 1024 or size > MAX_BINARY:
        raise PackageError(f"{path} is {size} bytes; an executable is between 1 KiB and 512 MiB, so this is truncated or not a build output.")
    return path.read_bytes()


def check_format(data: bytes, target: str) -> str:
    """Describe the executable, or refuse bytes that are not a TARGET executable."""
    system, arch = TARGETS[target]
    try:
        if system == "Linux":
            return check_elf(data, target, arch)
        if system == "Darwin":
            return check_macho(data, target, arch)
        return check_pe(data, target, arch)
    except struct.error:
        raise PackageError(f"The executable header is truncated, so this is not a complete {target} build.") from None


def check_elf(data: bytes, target: str, arch: str) -> str:
    if data[:4] != b"\x7fELF":
        raise PackageError(f"The binary is not a Linux (ELF) executable, so it cannot be the {target} build.")
    width, machine, label = ELF_MACHINES[arch]
    if data[4] != width or data[5] != 1 or data[6] != 1:
        raise PackageError(f"The binary is a {32 if data[4] == 1 else 64}-bit or big-endian ELF file; {target} needs a {32 if width == 1 else 64}-bit little-endian one.")
    file_type, found = struct.unpack_from("<HH", data, 16)
    if found != machine:
        raise PackageError(f"The binary is built for ELF machine {found}, not {label}; package it under its own target.")
    if file_type not in (2, 3):
        raise PackageError("The binary is an ELF object or core file, not an executable.")
    if width == 2:
        flags, = struct.unpack_from("<I", data, 48)
        phoff, = struct.unpack_from("<Q", data, 32)
        phentsize, phnum = struct.unpack_from("<HH", data, 54)
    else:
        flags, = struct.unpack_from("<I", data, 36)
        phoff, = struct.unpack_from("<I", data, 28)
        phentsize, phnum = struct.unpack_from("<HH", data, 42)
    if arch == "armv7hf" and flags & 0x200:
        raise PackageError("The binary uses the ARM soft-float ABI; linux-armv7hf needs a hard-float build (armv7-unknown-linux-musleabihf).")
    if phoff == 0 or phentsize < 8 or phoff + phentsize * phnum > len(data):
        raise PackageError("The ELF program headers are missing or truncated.")
    for index in range(phnum):
        kind, = struct.unpack_from("<I", data, phoff + index * phentsize)
        if kind == 3:  # PT_INTERP: the binary needs a dynamic loader from the system
            start = phoff + index * phentsize
            if width == 2:
                offset, = struct.unpack_from("<Q", data, start + 8)
                size, = struct.unpack_from("<Q", data, start + 32)
            else:
                offset, = struct.unpack_from("<I", data, start + 4)
                size, = struct.unpack_from("<I", data, start + 16)
            loader = data[offset:offset + size].split(b"\0")[0].decode(errors="replace")
            raise PackageError(f"The binary is dynamically linked (it asks for {loader}); Linux packages must be static so they run on every distribution. Build it for the *-unknown-linux-musl* target with cargo zigbuild.")
    return f"static {32 if width == 1 else 64}-bit ELF executable for {label}"


def check_macho(data: bytes, target: str, arch: str) -> str:
    if data[:4] in (b"\xca\xfe\xba\xbe", b"\xbe\xba\xfe\xca"):
        raise PackageError(f"The binary is a universal (fat) Mach-O file; package one architecture per target ({target}).")
    if data[:4] != b"\xcf\xfa\xed\xfe":
        raise PackageError(f"The binary is not a 64-bit macOS (Mach-O) executable, so it cannot be the {target} build.")
    cpu, _, file_type = struct.unpack_from("<III", data, 4)
    if cpu != MACHO_CPUS[arch]:
        raise PackageError(f"The binary is built for Mach-O CPU type {cpu:#x}, not {arch}; package it under its own target.")
    if file_type != 2:
        raise PackageError("The binary is a Mach-O library or object, not an executable.")
    commands, size = struct.unpack_from("<II", data, 16)
    if commands == 0 or 32 + size > len(data):
        raise PackageError("The Mach-O load commands are missing or truncated.")
    return f"64-bit Mach-O executable for {arch}"


def check_pe(data: bytes, target: str, arch: str) -> str:
    if data[:2] != b"MZ" or len(data) < 0x40:
        raise PackageError(f"The binary is not a Windows (PE) executable, so it cannot be the {target} build.")
    offset, = struct.unpack_from("<I", data, 0x3C)
    if offset < 0x40 or offset + 24 + 70 > len(data) or data[offset:offset + 4] != b"PE\0\0":
        raise PackageError("The Windows PE header is missing or truncated.")
    machine, = struct.unpack_from("<H", data, offset + 4)
    characteristics, = struct.unpack_from("<H", data, offset + 22)
    magic, = struct.unpack_from("<H", data, offset + 24)
    subsystem, = struct.unpack_from("<H", data, offset + 24 + 68)
    expected_machine, expected_magic = PE_MACHINES[arch]
    if machine != expected_machine or magic != expected_magic:
        raise PackageError(f"The binary is built for PE machine {machine:#x}, not {arch}; package it under its own target.")
    if not characteristics & 0x0002 or characteristics & 0x2000:
        raise PackageError("The binary is a Windows DLL or not marked executable.")
    if subsystem != 3:
        raise PackageError(f"The binary uses Windows subsystem {subsystem}, not the console; Silicon Apps reads the commands' output from a console program.")
    return f"{'PE32+' if magic == 0x20B else 'PE32'} console executable for {arch}"


def emulator_prefix() -> list[str]:
    raw = os.environ.get("PACKAGE_APPS_EMULATOR", "").strip()
    if not raw:
        return []
    if raw.startswith("["):
        try:
            value = json.loads(raw)
        except ValueError:
            value = None
        if not isinstance(value, list) or not value or not all(isinstance(part, str) and part for part in value):
            raise PackageError("PACKAGE_APPS_EMULATOR must be a command (qemu-arm) or a JSON list of strings.")
        return value
    return shlex.split(raw)


def clean_env(home: Path) -> dict[str, str]:
    """The environment Silicon Apps' runners give a package: an empty home and nothing else."""
    env = {"HOME": str(home), "SILICON_HOME": str(home), "TMPDIR": str(home), "LANG": "C", "APPS_TELEMETRY": "0"}
    if os.name == "nt":
        system_root = os.environ.get("SYSTEMROOT", r"C:\Windows")
        env.update({
            "PATH": system_root + "\\System32;" + system_root, "SYSTEMROOT": system_root, "WINDIR": system_root,
            "SYSTEMDRIVE": os.environ.get("SYSTEMDRIVE", "C:"), "PATHEXT": ".COM;.EXE;.BAT;.CMD",
            "USERPROFILE": str(home), "APPDATA": str(home), "LOCALAPPDATA": str(home), "TEMP": str(home), "TMP": str(home),
        })
    else:
        env["PATH"] = "/usr/bin:/bin"
    return env


def run(command: list[str], args: list[str], home: Path, timeout: int) -> tuple[int, str, str]:
    try:
        completed = subprocess.run([*command, *args], cwd=home, env=clean_env(home), stdin=subprocess.DEVNULL,
                                   capture_output=True, timeout=timeout)
    except subprocess.TimeoutExpired:
        raise PackageError(f"`{COMMAND} {' '.join(args)}` did not finish within {timeout} s in an empty home; Silicon Apps' runners stop it at 15 s.") from None
    except OSError as error:
        if error.errno in CANNOT_EXECUTE_ERRNOS or getattr(error, "winerror", None) in CANNOT_EXECUTE_WINERRORS:
            raise CannotRun(str(error)) from None
        if isinstance(error, FileNotFoundError) and len(command) > 1:
            raise PackageError(f"The emulator {command[0]!r} from PACKAGE_APPS_EMULATOR was not found; install it (qemu-user) or unset the variable.") from None
        raise PackageError(f"Could not start {command[-1]}: {error}") from None
    return completed.returncode, completed.stdout.decode(errors="replace"), completed.stderr.decode(errors="replace")


def json_object(text: str, what: str) -> dict:
    try:
        value = json.loads(text)
    except ValueError:
        raise PackageError(f"`{COMMAND} {what}` printed something that is not JSON: {text.strip()[:200]!r}") from None
    if not isinstance(value, dict):
        raise PackageError(f"`{COMMAND} {what}` printed JSON that is not an object: {text.strip()[:200]!r}")
    return value


def discovery(binary: Path, version: str) -> list[str]:
    """Run the commands Silicon Apps checks, signed out in an empty home. Raises CannotRun for a foreign format."""
    command = [*emulator_prefix(), str(binary)]
    timeout = 60 if len(command) > 1 else 30
    with tempfile.TemporaryDirectory(prefix="mcport-discovery-") as scratch:
        home = Path(scratch).resolve()

        def expect(args: list[str], code: int, out: str, err: str, ok: bool, wanted: str):
            if not ok:
                detail = (out.strip() or err.strip())[:400]
                raise PackageError(f"`{COMMAND} {' '.join(args)}` failed the Silicon Apps check in an empty home: expected {wanted}, got exit {code}: {detail!r}")

        code, out, err = run(command, ["--version"], home, timeout)
        expect(["--version"], code, out, err, code == 0 and out.strip() == f"{COMMAND} {version}", f"exit 0 and '{COMMAND} {version}'")
        code, out, err = run(command, ["--help"], home, timeout)
        expect(["--help"], code, out, err, code == 0 and bool(out.strip()), "exit 0 and help text")
        code, out, err = run(command, ["accounts", "--json"], home, timeout)
        expect(["accounts", "--json"], code, out, err, code == 0, "exit 0")
        accounts = json_object(out, "accounts --json")
        expect(["accounts", "--json"], code, out, err, accounts.get("app_id") == APP_ID, f'"app_id":"{APP_ID}"')
        if "version" in accounts:
            expect(["accounts", "--json"], code, out, err, accounts["version"] == version, f'"version":"{version}"')
        code, out, err = run(command, ["login", "status", "--json"], home, timeout)
        expect(["login", "status", "--json"], code, out, err, code == 0, "exit 0")
        status = json_object(out, "login status --json")
        expect(["login", "status", "--json"], code, out, err, status.get("authenticated") is False, '"authenticated":false')
        written = sorted(str(path.relative_to(home)) for path in home.rglob("*"))
        if written:
            raise PackageError(f"The discovery commands wrote into the empty home ({', '.join(written[:5])}); they must answer without creating state.")
    return ["--version", "--help", "accounts --json", "login status --json"]


def runnable_copy(data: bytes, target: str, directory: Path) -> Path:
    path = directory / binary_name(target)
    path.write_bytes(data)
    path.chmod(0o755)
    return path


def checked_record(path: Path, target: str, version: str, digest: str) -> dict:
    try:
        record = json.loads(path.read_text())
    except (OSError, ValueError) as error:
        raise PackageError(f"Cannot read the check record {path}: {error}") from None
    if not isinstance(record, dict):
        raise PackageError(f"The check record {path} is not a JSON object; pass the file --check-only --record wrote.")
    expected = {"app_id": APP_ID, "target": target, "version": version, "sha256": digest, "discovery": "passed"}
    wrong = {key: record.get(key) for key, value in expected.items() if record.get(key) != value}
    if wrong:
        raise PackageError(f"The check record {path} does not describe this binary ({json.dumps(wrong)}); pass the record its native check wrote for exactly these bytes.")
    return record


def render_manifest(version: str, target: str) -> str:
    lines = [line for line in TEMPLATE.read_text().splitlines() if not line.lstrip().startswith("#")]
    text = "\n".join(lines).strip() + "\n"
    text = text.replace("@VERSION@", version).replace("@TARGET@", target).replace("@BINARY@", f"bin/{binary_name(target)}")
    if "@" in text:
        raise PackageError(f"{TEMPLATE} has a placeholder this script does not fill: {text!r}")
    if f"app_id: {APP_ID}\n" not in text or f"command: {COMMAND}\n" not in text:
        raise PackageError(f"{TEMPLATE} must keep app_id {APP_ID} and command {COMMAND}.")
    return text


def find_silicon_apps(explicit: str | None) -> tuple[str, str]:
    candidates = [explicit, os.environ.get("SILICON_APPS"), shutil.which("silicon-apps")]
    fallback = Path.home() / ".apps" / "bin" / ("silicon-apps.exe" if os.name == "nt" else "silicon-apps")
    if fallback.is_file():
        candidates.append(str(fallback))
    path = next((candidate for candidate in candidates if candidate), None)
    if not path:
        raise PackageError("The Silicon Apps CLI is not installed: run `cargo install --locked silicon-apps-cli --version 0.2.0`, or pass --silicon-apps PATH.")
    try:
        completed = subprocess.run([path, "--version"], capture_output=True, text=True, timeout=30, env=apps_env(None))
    except (OSError, subprocess.TimeoutExpired) as error:
        raise PackageError(f"Cannot run the Silicon Apps CLI at {path}: {error}") from None
    found = re.fullmatch(r"silicon-apps (\d+)\.(\d+)\.(\d+)\S*", completed.stdout.strip())
    if completed.returncode != 0 or not found:
        raise PackageError(f"{path} is not the Silicon Apps CLI (`--version` printed {completed.stdout.strip()!r}).")
    if tuple(map(int, found.groups())) < MIN_SILICON_APPS:
        raise PackageError(f"{path} is {completed.stdout.strip()}; packaging needs silicon-apps 0.2.0 or newer.")
    return path, ".".join(found.groups())


def apps_env(home: Path | None) -> dict[str, str]:
    """validate and pack are local; never hand them a session, a recording key or the caller's Apps home."""
    env = {key: value for key, value in os.environ.items()
           if key not in {"APPS_TOKEN", "APPS_TELEMETRY_TABLE_KEY", "APPS_TELEMETRY_KEY", "APPS_URL", "ACCOUNTS_URL"}}
    env["APPS_TELEMETRY"] = "0"
    if home is not None:
        env.update({"SILICON_HOME": str(home), "HOME": str(home)})
        if os.name == "nt":
            env["USERPROFILE"] = str(home)
    return env


def silicon_apps(tool: str, args: list[str], home: Path) -> dict:
    completed = subprocess.run([tool, *args, "--json"], capture_output=True, text=True, timeout=300, env=apps_env(home))
    try:
        value = json.loads(completed.stdout or completed.stderr)
    except ValueError:
        value = {"error": {"message": (completed.stdout + completed.stderr).strip()}}
    if completed.returncode != 0 or (args[0] == "validate" and value.get("valid") is not True):
        errors = value.get("errors") or [value.get("error", {}).get("message", "no output")]
        raise PackageError(f"`silicon-apps {args[0]}` refused the package:\n  " + "\n  ".join(map(str, errors)))
    return value


def verify_archive(archive: Path, manifest: str, data: bytes, target: str, version: str, extract_to: Path) -> bool:
    """The archive holds exactly apps.yaml and the binary we checked. Returns whether discovery ran on the copy."""
    name = f"bin/{binary_name(target)}"
    with tarfile.open(archive, "r:gz") as bundle:
        members = {member.name: member for member in bundle.getmembers()}
        if set(members) != {"apps.yaml", name} or not all(member.isfile() for member in members.values()):
            raise PackageError(f"{archive.name} holds {sorted(members)}, not exactly apps.yaml and {name}.")
        if bundle.extractfile(members["apps.yaml"]).read().decode() != manifest:
            raise PackageError(f"{archive.name}: apps.yaml differs from the manifest that was validated.")
        packed = bundle.extractfile(members[name]).read()
        if packed != data:
            raise PackageError(f"{archive.name}: the packed binary is not the one that was checked.")
        if members[name].mode & 0o777 != 0o755:
            raise PackageError(f"{archive.name}: {name} has mode {members[name].mode & 0o777:o}, not 755.")
    copy = runnable_copy(packed, target, extract_to)
    try:
        discovery(copy, version)
        return True
    except CannotRun:
        return False


def check_only(args) -> dict:
    version, target = check_version(args.version), check_target(args.target)
    data = read_binary(args.binary)
    description = check_format(data, target)
    digest = hashlib.sha256(data).hexdigest()
    with tempfile.TemporaryDirectory(prefix="mcport-check-") as work:
        try:
            passed = discovery(runnable_copy(data, target, Path(work)), version)
        except CannotRun:
            raise PackageError(f"This machine ({platform.system()} {platform.machine()}) cannot run a {target} binary, so it cannot check it; run the check on a {target} runner or set PACKAGE_APPS_EMULATOR.") from None
    record = {"app_id": APP_ID, "target": target, "version": version, "sha256": digest, "discovery": "passed",
              "commands": passed, "format": description, "checked_on": f"{platform.system()} {platform.machine()}"}
    if args.record:
        args.record.parent.mkdir(parents=True, exist_ok=True)
        args.record.write_text(json.dumps(record, indent=2, sort_keys=True) + "\n")
    return record


def package(args) -> dict:
    version, target = check_version(args.version), check_target(args.target)
    data = read_binary(args.binary)
    description = check_format(data, target)
    digest = hashlib.sha256(data).hexdigest()
    output = args.output_dir.resolve()
    archive = output / f"{APP_ID}-{version}-{target}.tar.gz"
    checksum = archive.with_name(archive.name + ".sha256")
    if archive.exists() or checksum.exists():
        raise PackageError(f"{archive} already exists; packages are never overwritten. Remove it (and its .sha256) to package again.")
    tool, tool_version = find_silicon_apps(args.silicon_apps)
    record = checked_record(args.checked_record, target, version, digest) if args.checked_record else None
    manifest = render_manifest(version, target)
    with tempfile.TemporaryDirectory(prefix="mcport-package-") as work:
        work = Path(work).resolve()
        stage, home, extracted = work / "package", work / "apps-home", work / "extracted"
        for directory in (stage / "bin", home, extracted):
            directory.mkdir(parents=True)
        (stage / "apps.yaml").write_bytes(manifest.encode())
        staged = runnable_copy(data, target, stage / "bin")
        try:
            discovery(staged, version)
            checks = "passed here"
        except CannotRun:
            if record:
                checks = f"passed on {record.get('checked_on', 'a native runner')} (record)"
            elif args.require_discovery:
                raise PackageError(f"This machine cannot run a {target} binary and no --checked-record was given; --require-discovery refuses to pack an unchecked binary.") from None
            else:
                checks = f"not run: this machine ({platform.system()} {platform.machine()}) cannot run a {target} binary"
                print(f"package-apps: warning: discovery commands {checks}; check it on a {target} machine before uploading.", file=sys.stderr)
        silicon_apps(tool, ["validate", str(stage)], home)
        output.mkdir(parents=True, exist_ok=True)
        try:
            packed = silicon_apps(tool, ["pack", str(stage), "--output", str(archive)], home)
            archive_digest = hashlib.sha256(archive.read_bytes()).hexdigest()
            if packed.get("sha256") not in (None, archive_digest):
                raise PackageError(f"silicon-apps reported SHA-256 {packed.get('sha256')} for {archive.name}, but the file hashes to {archive_digest}.")
            archive_checked = verify_archive(archive, manifest, data, target, version, extracted)
        except BaseException:
            archive.unlink(missing_ok=True)
            raise
    checksum.write_text(f"{archive_digest}  {archive.name}\n")
    return {"archive": str(archive), "sha256": archive_digest, "app_id": APP_ID, "version": version, "target": target,
            "binary_sha256": digest, "format": description, "discovery": checks, "archive_checked": archive_checked,
            "silicon_apps": tool_version}


def main(argv: list[str] | None = None) -> int:
    parser = argparse.ArgumentParser(prog="package-apps.sh", description=__doc__.split("\n\n")[0],
                                     formatter_class=argparse.RawDescriptionHelpFormatter,
                                     epilog="Targets: " + ", ".join(TARGETS) + ". See scripts/README.md.")
    parser.add_argument("version", help="the workspace version (Cargo.toml), x.y.z")
    parser.add_argument("target", help="a Silicon Apps target, or 'host' with --check-only")
    parser.add_argument("binary", type=Path, help="the built mcport executable for that target")
    parser.add_argument("--check-only", action="store_true", help="run the binary checks on this machine and stop (CI's native runners)")
    parser.add_argument("--record", type=Path, help="with --check-only: write the check record (JSON) here")
    parser.add_argument("--checked-record", type=Path, help="a record from --check-only on a native runner, for binaries this machine cannot run")
    parser.add_argument("--require-discovery", action="store_true", help="refuse to pack unless the commands ran here or a matching record is given")
    parser.add_argument("--output-dir", type=Path, default=DEFAULT_OUTPUT, help="where archives go (default dist/apps)")
    parser.add_argument("--silicon-apps", help="the silicon-apps CLI (default: SILICON_APPS, PATH, then ~/.apps/bin)")
    args = parser.parse_args(argv)
    if args.target == "host" and not args.check_only:
        parser.error("'host' is only accepted with --check-only; packages name their target.")
    if args.record and not args.check_only:
        parser.error("--record goes with --check-only.")
    try:
        result = check_only(args) if args.check_only else package(args)
    except PackageError as error:
        print(f"package-apps: {error}", file=sys.stderr)
        return 1
    print(json.dumps(result, sort_keys=True))
    return 0


if __name__ == "__main__":
    sys.exit(main())
