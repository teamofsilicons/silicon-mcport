"""Rules of scripts/package_apps.py. Header fixtures and shell-script stand-ins are test inputs only, never packages."""
import argparse
import contextlib
import hashlib
import importlib.util
import io
import json
import os
from pathlib import Path
import platform
import struct
import tarfile
import tempfile
import unittest
from unittest import mock

SPEC = importlib.util.spec_from_file_location("package_apps", Path(__file__).parents[1] / "package_apps.py")
package_apps = importlib.util.module_from_spec(SPEC)
SPEC.loader.exec_module(package_apps)
VERSION = package_apps.workspace_version()


def header(target, *, dynamic=False, soft_float=False, fat=False, dll=False, gui=False):
    """A 2 KiB file whose header claims TARGET; enough for the format rules, never runnable."""
    data = bytearray(2048)
    system, arch = package_apps.TARGETS[target]
    if system == "Linux":
        width, machine, _ = package_apps.ELF_MACHINES[arch]
        data[:7] = b"\x7fELF" + bytes([width, 1, 1])
        struct.pack_into("<HH", data, 16, 2, machine)
        if width == 2:
            struct.pack_into("<Q", data, 32, 64)
            struct.pack_into("<HH", data, 54, 56, 1)
            phdr = 64
        else:
            struct.pack_into("<I", data, 28, 52)
            struct.pack_into("<I", data, 36, 0x05000200 if soft_float else 0x05000400)
            struct.pack_into("<HH", data, 42, 32, 1)
            phdr = 52
        struct.pack_into("<I", data, phdr, 3 if dynamic else 1)
        if dynamic:
            loader = b"/lib/ld-linux.so.2\0"
            data[512:512 + len(loader)] = loader
            if width == 2:
                struct.pack_into("<Q", data, phdr + 8, 512)
                struct.pack_into("<Q", data, phdr + 32, len(loader))
            else:
                struct.pack_into("<I", data, phdr + 4, 512)
                struct.pack_into("<I", data, phdr + 16, len(loader))
    elif system == "Darwin":
        data[:4] = b"\xca\xfe\xba\xbe" if fat else b"\xcf\xfa\xed\xfe"
        struct.pack_into("<IIIII", data, 4, package_apps.MACHO_CPUS[arch], 0, 2, 1, 72)
    else:
        machine, magic = package_apps.PE_MACHINES[arch]
        data[:2] = b"MZ"
        struct.pack_into("<I", data, 0x3C, 128)
        data[128:132] = b"PE\0\0"
        struct.pack_into("<H", data, 132, machine)
        struct.pack_into("<H", data, 150, 0x2022 if dll else 0x0022)
        struct.pack_into("<H", data, 152, magic)
        struct.pack_into("<H", data, 152 + 68, 2 if gui else 3)
    return bytes(data)


FAKE_CLI = """#!/bin/sh
case "$*" in
  "--version") echo "mcport {version}" ;;
  "--help") echo "mcport: use MCP connections from any machine" ;;
  "accounts --json") echo '{{"app_id":"{app_id}","version":"{version}"}}' ;;
  "login status --json") {status} ;;
  *) echo "unknown: $*" >&2; exit 2 ;;
esac
""" + "# A stand-in for the mcport executable in packaging tests; padded past the 1 KiB executable minimum.\n" * 12


def fake_cli(directory, *, app_id="mcport", version=VERSION, status="echo '{\"authenticated\":false}'"):
    path = Path(directory) / "mcport"
    path.write_text(FAKE_CLI.format(app_id=app_id, version=version, status=status))
    path.chmod(0o755)
    return path


class FormatTests(unittest.TestCase):
    def test_every_target_accepts_its_own_header_and_refuses_the_others(self):
        for target in package_apps.TARGETS:
            with self.subTest(target=target):
                self.assertIn("executable", package_apps.check_format(header(target), target))
                for other in package_apps.TARGETS:
                    if other != target:
                        with self.assertRaises(package_apps.PackageError):
                            package_apps.check_format(header(target), other)

    def test_truncated_and_empty_files_are_refused(self):
        for target in package_apps.TARGETS:
            with self.subTest(target=target), self.assertRaises(package_apps.PackageError):
                package_apps.check_format(header(target)[:40], target)
        with tempfile.TemporaryDirectory() as directory:
            path = Path(directory) / "mcport"
            path.write_bytes(b"\x7fELF")
            with self.assertRaisesRegex(package_apps.PackageError, "truncated"):
                package_apps.read_binary(path)
            with self.assertRaisesRegex(package_apps.PackageError, "regular file"):
                package_apps.read_binary(Path(directory) / "missing")

    def test_linux_packages_must_be_static(self):
        for target in ("linux-x86_64", "linux-i686", "linux-aarch64", "linux-armv7hf"):
            with self.subTest(target=target), self.assertRaisesRegex(package_apps.PackageError, "ld-linux.so.2.*musl"):
                package_apps.check_format(header(target, dynamic=True), target)

    def test_development_checks_may_accept_a_dynamic_build_but_packages_never_do(self):
        description = package_apps.check_format(header("linux-x86_64", dynamic=True), "linux-x86_64", allow_dynamic=True)
        self.assertIn("development build", description)
        errors = io.StringIO()
        with contextlib.redirect_stderr(errors), self.assertRaises(SystemExit):
            package_apps.main([VERSION, "linux-x86_64", "mcport", "--allow-dynamic"])
        self.assertIn("packages are always static", errors.getvalue())

    def test_armv7_needs_the_hard_float_abi(self):
        with self.assertRaisesRegex(package_apps.PackageError, "soft-float"):
            package_apps.check_format(header("linux-armv7hf", soft_float=True), "linux-armv7hf")

    def test_universal_mach_o_dll_and_gui_programs_are_refused(self):
        with self.assertRaisesRegex(package_apps.PackageError, "universal"):
            package_apps.check_format(header("macos-aarch64", fat=True), "macos-aarch64")
        with self.assertRaisesRegex(package_apps.PackageError, "DLL"):
            package_apps.check_format(header("windows-x86_64", dll=True), "windows-x86_64")
        with self.assertRaisesRegex(package_apps.PackageError, "console"):
            package_apps.check_format(header("windows-aarch64", gui=True), "windows-aarch64")


class ManifestAndArgumentTests(unittest.TestCase):
    def test_manifest_lists_only_its_target_and_the_workspace_version(self):
        text = package_apps.render_manifest(VERSION, "linux-armv7hf")
        self.assertEqual(text, f"schema_version: 1\napp_id: mcport\nversion: {VERSION}\ncommand: mcport\ntargets:\n  linux-armv7hf:\n    binary: bin/mcport\n")
        self.assertIn("binary: bin/mcport.exe\n", package_apps.render_manifest(VERSION, "windows-x86_64"))
        self.assertNotIn("#", text)

    def test_versions_must_be_plain_and_match_the_workspace(self):
        for version in ("0.3", "v0.3.0", "0.3.0-rc.1", "0.3.0+build", "01.3.0"):
            with self.subTest(version=version), self.assertRaisesRegex(package_apps.PackageError, "plain x.y.z"):
                package_apps.check_version(version)
        with self.assertRaisesRegex(package_apps.PackageError, "workspace version"):
            package_apps.check_version("99.0.0")
        self.assertEqual(package_apps.check_version(VERSION), VERSION)

    def test_targets_are_the_silicon_apps_names(self):
        with self.assertRaisesRegex(package_apps.PackageError, "Unknown target"):
            package_apps.check_target("x86_64-unknown-linux-musl")
        self.assertIn(package_apps.check_target("host"), package_apps.TARGETS)

    def test_host_is_only_for_checks_and_records_only_for_checks(self):
        for argv, message in (([VERSION, "host", "mcport"], "only accepted with --check-only"),
                              ([VERSION, "linux-x86_64", "mcport", "--record", "x.json"], "--record goes with --check-only")):
            errors = io.StringIO()
            with self.subTest(argv=argv), contextlib.redirect_stderr(errors), self.assertRaises(SystemExit):
                package_apps.main(argv)
            self.assertIn(message, errors.getvalue())

    def test_emulator_setting(self):
        with mock.patch.dict(os.environ, {"PACKAGE_APPS_EMULATOR": "qemu-arm -cpu cortex-a7"}):
            self.assertEqual(package_apps.emulator_prefix(), ["qemu-arm", "-cpu", "cortex-a7"])
        with mock.patch.dict(os.environ, {"PACKAGE_APPS_EMULATOR": '["qemu-aarch64"]'}):
            self.assertEqual(package_apps.emulator_prefix(), ["qemu-aarch64"])
        for raw in ('["", 3]', "[not json"):
            with self.subTest(raw=raw), mock.patch.dict(os.environ, {"PACKAGE_APPS_EMULATOR": raw}), self.assertRaises(package_apps.PackageError):
                package_apps.emulator_prefix()


@unittest.skipIf(os.name == "nt", "the stand-in CLI is a POSIX shell script")
class DiscoveryTests(unittest.TestCase):
    def setUp(self):
        self.directory = tempfile.TemporaryDirectory()
        self.path = Path(self.directory.name)

    def tearDown(self):
        self.directory.cleanup()

    def test_a_cli_that_answers_like_silicon_apps_expects_passes(self):
        self.assertEqual(package_apps.discovery(fake_cli(self.path), VERSION), ["--version", "--help", "accounts --json", "login status --json"])

    def test_each_wrong_answer_is_refused_with_the_command_and_the_expectation(self):
        cases = {
            "app_id": ({"app_id": "remind"}, '"app_id":"mcport"'),
            "version": ({"version": "0.0.1"}, "mcport 0.0.1"),
            "signed in": ({"status": "echo '{\"authenticated\":true}'"}, '"authenticated":false'),
            "exit 1": ({"status": "echo '{\"authenticated\":false}'; exit 1"}, "login status --json.*exit 0"),
            "not json": ({"status": "echo signed out"}, "not JSON"),
        }
        for name, (options, message) in cases.items():
            with self.subTest(name), tempfile.TemporaryDirectory() as directory:
                with self.assertRaisesRegex(package_apps.PackageError, message):
                    package_apps.discovery(fake_cli(directory, **options), VERSION)

    def test_answers_run_in_an_empty_home_and_must_not_write_there(self):
        status = 'echo "$HOME|$SILICON_HOME|$TMPDIR|$APPS_TELEMETRY|$PATH" > "$HOME/seen"; echo \'{"authenticated":false}\''
        with self.assertRaisesRegex(package_apps.PackageError, "wrote into the empty home \\(seen\\)"):
            package_apps.discovery(fake_cli(self.path, status=status), VERSION)

    def test_a_format_this_machine_cannot_execute_is_reported_as_such(self):
        foreign = "linux-x86_64" if platform.system() == "Darwin" else "macos-aarch64"
        path = self.path / "mcport"
        path.write_bytes(header(foreign))
        path.chmod(0o755)
        with self.assertRaises(package_apps.CannotRun):
            package_apps.discovery(path, VERSION)

    def test_a_missing_emulator_is_named(self):
        with mock.patch.dict(os.environ, {"PACKAGE_APPS_EMULATOR": "qemu-does-not-exist"}):
            with self.assertRaisesRegex(package_apps.PackageError, "qemu-does-not-exist"):
                package_apps.discovery(fake_cli(self.path), VERSION)


def silicon_apps_or_none():
    try:
        return package_apps.find_silicon_apps(None)[0]
    except package_apps.PackageError:
        return None


@unittest.skipIf(os.name == "nt", "the stand-in CLI is a POSIX shell script")
@unittest.skipUnless(silicon_apps_or_none(), "needs the silicon-apps CLI (cargo install --locked silicon-apps-cli --version 0.2.0)")
class PackageTests(unittest.TestCase):
    """End to end through the real `silicon-apps validate` and `pack`; only the format rule is relaxed for the stand-in."""

    def setUp(self):
        self.directory = tempfile.TemporaryDirectory()
        self.path = Path(self.directory.name)
        self.target = package_apps.host_target()
        self.binary = fake_cli(self.path)
        self.output = self.path / "dist"
        patcher = mock.patch.object(package_apps, "check_format", return_value="stand-in")
        patcher.start()
        self.addCleanup(patcher.stop)

    def tearDown(self):
        self.directory.cleanup()

    def args(self, **options):
        values = {"version": VERSION, "target": self.target, "binary": self.binary, "check_only": False, "record": None,
                  "checked_record": None, "require_discovery": False, "output_dir": self.output, "silicon_apps": None}
        values.update(options)
        return argparse.Namespace(**values)

    def test_one_archive_per_target_with_its_checksum_and_never_overwritten(self):
        result = package_apps.package(self.args())
        archive = self.output / f"mcport-{VERSION}-{self.target}.tar.gz"
        self.assertEqual(Path(result["archive"]), archive.resolve())
        self.assertEqual(result["discovery"], "passed here")
        self.assertTrue(result["archive_checked"])
        digest = hashlib.sha256(archive.read_bytes()).hexdigest()
        self.assertEqual((self.output / (archive.name + ".sha256")).read_text(), f"{digest}  {archive.name}\n")
        with tarfile.open(archive) as bundle:
            self.assertEqual(sorted(bundle.getnames()), ["apps.yaml", "bin/mcport"])
            self.assertEqual(bundle.getmember("bin/mcport").mode, 0o755)
            self.assertEqual(bundle.extractfile("apps.yaml").read().decode(), package_apps.render_manifest(VERSION, self.target))
        with self.assertRaisesRegex(package_apps.PackageError, "never overwritten"):
            package_apps.package(self.args())
        self.assertEqual(hashlib.sha256(archive.read_bytes()).hexdigest(), digest)

    def test_a_binary_that_fails_discovery_is_never_packed(self):
        self.binary = fake_cli(self.path, status="echo '{\"authenticated\":true}'")
        with self.assertRaisesRegex(package_apps.PackageError, "authenticated"):
            package_apps.package(self.args())
        self.assertFalse(self.output.exists() and any(self.output.iterdir()))

    def test_unrunnable_binaries_need_a_matching_record_when_discovery_is_required(self):
        digest = hashlib.sha256(self.binary.read_bytes()).hexdigest()
        record = self.path / "record.json"
        with mock.patch.object(package_apps, "discovery", side_effect=package_apps.CannotRun("exec format error")):
            with self.assertRaisesRegex(package_apps.PackageError, "no --checked-record"):
                package_apps.package(self.args(require_discovery=True))
            record.write_text("[]")
            with self.assertRaisesRegex(package_apps.PackageError, "not a JSON object"):
                package_apps.package(self.args(checked_record=record, require_discovery=True))
            record.write_text(json.dumps({"app_id": "mcport", "target": self.target, "version": VERSION, "sha256": "0" * 64, "discovery": "passed"}))
            with self.assertRaisesRegex(package_apps.PackageError, "does not describe this binary"):
                package_apps.package(self.args(checked_record=record, require_discovery=True))
            record.write_text(json.dumps({"app_id": "mcport", "target": self.target, "version": VERSION, "sha256": digest, "discovery": "passed", "checked_on": "Linux aarch64"}))
            result = package_apps.package(self.args(checked_record=record, require_discovery=True))
        self.assertEqual(result["discovery"], "passed on Linux aarch64 (record)")
        self.assertFalse(result["archive_checked"])


if __name__ == "__main__":
    unittest.main()
