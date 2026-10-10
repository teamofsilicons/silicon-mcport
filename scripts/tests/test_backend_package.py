"""Synthetic bytes exercise rejection rules; they are never native-build evidence."""
import hashlib
import importlib.util
import io
import json
import os
from pathlib import Path
import socket
import struct
import subprocess
import sys
import tarfile
import tempfile
import unittest
from unittest import mock
from contextlib import ExitStack, nullcontext
from types import SimpleNamespace

ROOT = Path(__file__).resolve().parents[2]
spec = importlib.util.spec_from_file_location("package_backend", ROOT / "scripts/package_backend.py")
backend = importlib.util.module_from_spec(spec)
spec.loader.exec_module(backend)
REVISION = "a" * 40


class BackendPackageTests(unittest.TestCase):
    def setUp(self):
        self.temporary = tempfile.TemporaryDirectory()
        self.path = Path(self.temporary.name).resolve()
        self.binary = self.path / "mcport-server"
        elf = bytearray(1024)
        elf[:7] = b"\x7fELF\x02\x01\x01"
        struct.pack_into("<HH", elf, 16, 3, 183)
        struct.pack_into("<H", elf, 52, 64)
        self.binary.write_bytes(elf)
        self.binary.chmod(0o755)
        self.state = self.path / "state-fixture"
        self.state.mkdir()
        self.evidence = self.path / "provenance.json"
        self.evidence.write_text(json.dumps({"source_revision": REVISION, "target": "aarch64-unknown-linux-gnu", "build_distribution": "Amazon Linux 2023", "glibc_baseline": "2.34", "build_image": "public.ecr.aws/amazonlinux/amazonlinux@sha256:" + "b" * 64, "rustc": "rustc fixture", "binary_sha256": hashlib.sha256(elf).hexdigest(), "native_tests_passed": True, "native_health_smoke_passed": True, "health_version": backend.tomllib.loads((ROOT / "Cargo.toml").read_text())["workspace"]["package"]["version"], "health_source_revision": REVISION}))

    def tearDown(self):
        self.temporary.cleanup()

    def package(self, name="candidate.tar.gz"):
        output = self.path / name
        result = backend.package(self.binary, self.evidence, REVISION, output)
        return output, result

    def rebuild(self, output, mutate):
        with tarfile.open(output, "r:gz") as archive:
            entries = [(member, archive.extractfile(member).read()) for member in archive]
        destination = self.path / "modified.tar.gz"
        with tarfile.open(destination, "w:gz") as archive:
            for member, data in mutate(entries):
                archive.addfile(member, io.BytesIO(data))
        return destination, hashlib.sha256(destination.read_bytes()).hexdigest()

    def test_complete_deterministic_bundle_and_readonly_installer(self):
        output, result = self.package()
        second, _ = self.package("second.tar.gz")
        self.assertEqual(output.read_bytes(), second.read_bytes())
        metadata, members = backend.installer.validate_bundle(output, result["sha256"], REVISION)
        self.assertEqual(metadata["source_revision"], REVISION)
        self.assertEqual(set(members), backend.installer.FIXED_FILES)
        before = set(self.path.iterdir())
        run = subprocess.run([sys.executable, str(ROOT / "deploy/install.py"), "--bundle", str(output), "--sha256", result["sha256"], "--revision", REVISION], capture_output=True, text=True, check=True)
        self.assertFalse(json.loads(run.stdout)["apply"])
        self.assertEqual(before, set(self.path.iterdir()))
        with self.assertRaises(FileExistsError):
            self.package()

    def test_wrong_architecture_and_wrong_build_evidence_rejected(self):
        data = bytearray(self.binary.read_bytes())
        struct.pack_into("<H", data, 18, 62)
        self.binary.write_bytes(data)
        with self.assertRaisesRegex(ValueError, "ARM64"):
            self.package()
        struct.pack_into("<H", data, 18, 183)
        self.binary.write_bytes(data)
        evidence = json.loads(self.evidence.read_text())
        evidence["native_tests_passed"] = False
        self.evidence.write_text(json.dumps(evidence))
        with self.assertRaisesRegex(ValueError, "evidence"):
            self.package()

    def test_changed_payload_revision_or_archive_digest_rejected(self):
        output, result = self.package()
        with self.assertRaisesRegex(ValueError, "SHA-256"):
            backend.installer.validate_bundle(output, "0" * 64, REVISION)
        with self.assertRaisesRegex(ValueError, "provenance"):
            backend.installer.validate_bundle(output, result["sha256"], "c" * 40)
        changed, digest = self.rebuild(output, lambda entries: [(member, data.replace(b"#", b"!", 1) if member.name == "README.md" else data) for member, data in entries])
        with self.assertRaisesRegex(ValueError, "checksum"):
            backend.installer.validate_bundle(changed, digest, REVISION)

    def test_archive_traversal_symlink_and_duplicate_rejected(self):
        output, _ = self.package()
        for kind in ("traversal", "symlink", "duplicate"):
            with self.subTest(kind=kind):
                def mutate(entries):
                    member = tarfile.TarInfo("../outside" if kind == "traversal" else "link.js")
                    member.mode = 0o644
                    if kind == "symlink":
                        member.type, member.linkname = tarfile.SYMTYPE, "/etc/passwd"
                    return entries + ([entries[0]] if kind == "duplicate" else [(member, b"")])
                changed, digest = self.rebuild(output, mutate)
                with self.assertRaisesRegex(ValueError, "unsafe"):
                    backend.installer.validate_bundle(changed, digest, REVISION)

    def test_bundles_that_still_carry_a_website_are_refused(self):
        # The website deploys separately; an older bundle layout must not install half-used.
        output, _ = self.package()
        for name in ("web/dist/index.html", ".env"):
            with self.subTest(name=name):
                def mutate(entries):
                    member = tarfile.TarInfo(name)
                    member.mode, member.size = 0o644, 9
                    return entries + [(member, b"<html/>\n\n")]
                changed, digest = self.rebuild(output, mutate)
                with self.assertRaisesRegex(ValueError, "unsafe"):
                    backend.installer.validate_bundle(changed, digest, REVISION)
        self.assertFalse(backend.installer.allowed_name("web/dist/index.html"))

    @unittest.skipIf(os.name == "nt", "Windows developer symlink privilege varies")
    def test_state_symlinks_rejected(self):
        (self.state / "link.js").symlink_to(self.evidence)
        with self.assertRaisesRegex(ValueError, "symlinks"):
            backend.installer.validate_state_tree(self.state)

    @unittest.skipIf(os.name == "nt", "Installer is a Unix systemd host operation")
    def test_failed_cutover_restores_previous_release_and_leaves_service_stopped(self):
        self.simulate_cutover(failure="health")

    @unittest.skipIf(os.name == "nt", "Installer is a Unix systemd host operation")
    def test_unconfirmed_stop_preserves_candidate_and_refuses_data_restore(self):
        self.simulate_cutover(failure="health", stop_fails=True)

    @unittest.skipIf(os.name == "nt", "Installer is a Unix systemd host operation")
    def test_telemetry_socket_upgrade_preserves_durable_state_and_runtime(self):
        self.simulate_cutover()

    @unittest.skipIf(os.name == "nt", "Installer is a Unix systemd host operation")
    def test_inactive_service_with_main_pid_refuses_socket_cleanup_and_switch(self):
        self.simulate_cutover(stopped_pid=321)

    @unittest.skipIf(os.name == "nt", "Installer is a Unix systemd host operation")
    def test_backup_failure_after_socket_cleanup_restarts_previous_service(self):
        self.simulate_cutover(failure="backup")

    def create_socket(self, path):
        path.parent.mkdir(parents=True, exist_ok=True)
        # Bind a short relative name to stay below AF_UNIX's platform path limit.
        previous = Path.cwd()
        try:
            os.chdir(path.parent)
            with socket.socket(socket.AF_UNIX, socket.SOCK_STREAM) as connection:
                connection.bind(path.name)
        finally:
            os.chdir(previous)

    @unittest.skipIf(os.name == "nt", "Unix socket and ownership validation")
    def test_only_exact_owned_telemetry_socket_is_allowed_during_preflight(self):
        state = self.path / "state"
        ipc = state / "telemetry" / ("a" * 64) / "daemon.sock"
        self.create_socket(ipc)
        original = ipc.lstat()
        accepted = backend.installer.validate_state_tree(state, os.getuid())
        self.assertEqual([entry[0] for entry in accepted], [ipc])
        self.assertEqual(ipc.lstat().st_ino, original.st_ino)
        for uid in (None, os.getuid() + 1):
            with self.subTest(uid=uid), self.assertRaises(ValueError):
                backend.installer.validate_state_tree(state, uid)
        self.assertTrue(ipc.exists())

    @unittest.skipIf(os.name == "nt", "Unix special files and symlink validation")
    def test_invalid_socket_locations_and_other_special_entries_are_rejected(self):
        valid = "telemetry/" + "a" * 64 + "/daemon.sock"
        cases = [
            ("uppercase", "telemetry/" + "A" * 64 + "/daemon.sock", "socket"),
            ("short", "telemetry/" + "a" * 63 + "/daemon.sock", "socket"),
            ("nested", "telemetry/" + "a" * 64 + "/nested/daemon.sock", "socket"),
            ("other_root", "other/" + "a" * 64 + "/daemon.sock", "socket"),
            ("other_name", "telemetry/" + "a" * 64 + "/other.sock", "socket"),
            ("fifo", valid, "fifo"),
            ("symlink", valid, "symlink"),
        ]
        for name, relative, kind in cases:
            with self.subTest(name=name):
                state = self.path / name
                entry = state / relative
                entry.parent.mkdir(parents=True)
                if kind == "socket":
                    self.create_socket(entry)
                elif kind == "fifo":
                    os.mkfifo(entry)
                else:
                    entry.symlink_to(self.evidence)
                with self.assertRaises(ValueError):
                    backend.installer.validate_state_tree(state, os.getuid())
                self.assertTrue(entry.exists())
        state = self.path / "directory_symlink"
        state.mkdir()
        (state / "telemetry").symlink_to(self.path / "fifo/telemetry", target_is_directory=True)
        with self.assertRaises(ValueError):
            backend.installer.validate_state_tree(state, os.getuid())

    @unittest.skipIf(os.name == "nt", "Unix socket validation")
    def test_unsafe_state_prevents_partial_socket_cleanup(self):
        state = self.path / "state"
        ipc = state / "telemetry" / ("a" * 64) / "daemon.sock"
        self.create_socket(ipc)
        os.mkfifo(state / "unexpected")
        with mock.patch.object(backend.installer, "service_stopped", return_value=True):
            with self.assertRaises(ValueError):
                backend.installer.remove_stopped_telemetry_sockets(state, os.getuid())
        self.assertTrue(ipc.exists())

    @unittest.skipIf(os.name == "nt", "Unix socket validation")
    def test_changed_socket_is_not_unlinked(self):
        state = self.path / "state"
        ipc = state / "telemetry" / ("a" * 64) / "daemon.sock"
        self.create_socket(ipc)
        validate = backend.installer.validate_state_tree
        def replace_after_validation(path, uid):
            entries = validate(path, uid)
            ipc.unlink()
            ipc.write_bytes(b"replacement-must-survive")
            return entries
        with mock.patch.object(backend.installer, "service_stopped", return_value=True), mock.patch.object(
            backend.installer, "validate_state_tree", side_effect=replace_after_validation
        ):
            with self.assertRaisesRegex(RuntimeError, "changed"):
                backend.installer.remove_stopped_telemetry_sockets(state, os.getuid())
        self.assertEqual(ipc.read_bytes(), b"replacement-must-survive")

    @unittest.skipIf(os.name == "nt", "Unix permission bits are required for this installer regression")
    def test_release_directories_are_traversable_under_private_umask(self):
        output, result = self.package()
        metadata, members = backend.installer.validate_bundle(output, result["sha256"], REVISION)
        prefix = self.path / "opt"
        releases = prefix / "releases"
        releases.mkdir(mode=0o700, parents=True)
        releases.chmod(0o700)
        private = self.path / "private"
        private.mkdir(mode=0o700)
        secret = private / "runtime.env"
        secret.write_text("FIXTURE_ONLY=placeholder")
        secret.chmod(0o600)
        release = releases / REVISION
        previous_umask = os.umask(0o077)
        try:
            with mock.patch.object(backend.installer, "PREFIX", prefix), mock.patch.object(
                backend.installer, "command", side_effect=RuntimeError("stop-before-host-operations")
            ):
                with self.assertRaisesRegex(RuntimeError, "stop-before-host-operations"):
                    backend.installer.install_release(metadata, members, "https://fixture.example/health", release, None)
        finally:
            os.umask(previous_umask)
        self.assertEqual(release.stat().st_mode & 0o777, 0o755, str(release))
        self.assertEqual((release / "README.md").stat().st_mode & 0o777, 0o644)
        self.assertEqual((release / "mcport-server").stat().st_mode & 0o777, 0o755)
        self.assertEqual(releases.stat().st_mode & 0o777, 0o700)
        self.assertEqual(private.stat().st_mode & 0o777, 0o700)
        self.assertEqual(secret.stat().st_mode & 0o777, 0o600)

    @unittest.skipIf(os.name == "nt", "Unix permission bits are required for this installer regression")
    def test_new_public_paths_override_umask_without_broadening_existing_private_paths(self):
        prefix = self.path / "opt"
        releases = prefix / "releases"
        backups = self.path / "backups"
        private = self.path / "private"
        private.mkdir(mode=0o700)
        secret = private / "runtime.env"
        secret.write_text("FIXTURE_ONLY=placeholder")
        secret.chmod(0o600)
        real_stat = Path.stat
        def root_owned_stat(path, *args, **kwargs):
            info = real_stat(path, *args, **kwargs)
            return SimpleNamespace(st_mode=info.st_mode, st_uid=0)
        previous_umask = os.umask(0o077)
        try:
            with mock.patch.object(Path, "stat", root_owned_stat):
                backend.installer.root_directory(prefix)
                backend.installer.root_directory(releases)
                backend.installer.root_directory(backups, private=True)
                backend.installer.root_directory(private, private=True)
                with self.assertRaisesRegex(ValueError, "traversable"):
                    backend.installer.root_directory(private)
        finally:
            os.umask(previous_umask)
        self.assertEqual(prefix.stat().st_mode & 0o777, 0o755)
        self.assertEqual(releases.stat().st_mode & 0o777, 0o755)
        self.assertEqual(backups.stat().st_mode & 0o777, 0o700)
        self.assertEqual(private.stat().st_mode & 0o777, 0o700)
        self.assertEqual(secret.stat().st_mode & 0o777, 0o600)

    def simulate_cutover(self, failure=None, stop_fails=False, stopped_pid=0):
        import fcntl
        output, result = self.package()
        metadata, members = backend.installer.validate_bundle(output, result["sha256"], REVISION)
        prefix, data, runtime, unit, backups = [self.path / name for name in ("opt", "data", "runtime.env", "mcport.service", "backups")]
        previous = prefix / "releases" / ("c" * 40)
        previous.mkdir(parents=True)
        (prefix / "current").symlink_to(previous)
        data.mkdir(mode=0o700)
        ipc = data / "telemetry" / ("a" * 64) / "daemon.sock"
        self.create_socket(ipc)
        durable = {
            "data.sqlite": b"fixture-state-not-a-database",
            "master.key": b"fixture-master-key",
            "telemetry/" + "a" * 64 + "/spool.jsonl": b'{"fixture":true}\n',
            "telemetry/" + "a" * 64 + "/cursor": b"14\n",
            # An ordinary file named daemon.sock is data, never ephemeral IPC.
            "telemetry/" + "b" * 64 + "/daemon.sock": b"ordinary-state-file",
        }
        for relative, contents in durable.items():
            entry = data / relative
            entry.parent.mkdir(parents=True, exist_ok=True)
            entry.write_bytes(contents)
            entry.chmod(0o600)
        runtime_contents = "MCPORT_DATA_DIR=/var/lib/mcport\nMCPORT_BIND=127.0.0.1:4380\nFIXTURE_ONLY=placeholder"
        runtime.write_text(runtime_contents)
        runtime.chmod(0o600)
        unit.write_text("previous-unit-fixture")
        commands, service_state, stop_count = [], ["active"], [0]
        def fake_command(*args, **_):
            commands.append(args)
            if args[:2] == ("systemctl", "stop"):
                stop_count[0] += 1
                if not stop_fails or stop_count[0] == 1:
                    service_state[0] = "inactive"
            if args[:2] == ("systemctl", "start"):
                service_state[0] = "active"
            stdout = b""
            if "--property=ActiveState" in args:
                stdout = service_state[0].encode()
            if "--property=MainPID" in args:
                stdout = str(123 if service_state[0] == "active" else stopped_pid).encode()
            if "--property=FragmentPath" in args:
                stdout = str(unit).encode()
            return subprocess.CompletedProcess(args, 0, stdout=stdout, stderr=b"")
        real_unlink = Path.unlink
        removals = []
        def checked_unlink(path, *args, **kwargs):
            if path == ipc:
                self.assertEqual(service_state[0], "inactive")
                self.assertEqual(stopped_pid, 0)
                with (prefix / "install.lock").open("a") as probe:
                    with self.assertRaises(BlockingIOError):
                        fcntl.flock(probe, fcntl.LOCK_EX | fcntl.LOCK_NB)
                removals.append(path)
            return real_unlink(path, *args, **kwargs)
        with ExitStack() as stack:
            for name, value in {"PREFIX": prefix, "DATA": data, "RUNTIME": runtime, "UNIT": unit, "BACKUPS": backups}.items():
                stack.enter_context(mock.patch.object(backend.installer, name, value))
            stack.enter_context(mock.patch.object(backend.installer.os, "geteuid", return_value=0))
            stack.enter_context(mock.patch.object(backend.installer.platform, "system", return_value="Linux"))
            stack.enter_context(mock.patch.object(backend.installer.platform, "machine", return_value="aarch64"))
            stack.enter_context(mock.patch("pwd.getpwnam", return_value=SimpleNamespace(pw_uid=os.getuid())))
            stack.enter_context(mock.patch.object(backend.installer, "regular_private"))
            stack.enter_context(mock.patch.object(backend.installer, "root_directory", side_effect=lambda path, **_: path.mkdir(parents=True, exist_ok=True)))
            stack.enter_context(mock.patch.object(backend.installer, "command", side_effect=fake_command))
            stack.enter_context(mock.patch.object(Path, "unlink", checked_unlink))
            stack.enter_context(mock.patch.object(backend.installer, "health", side_effect=RuntimeError("health-failed-fixture") if failure == "health" else None))
            if failure == "backup":
                stack.enter_context(mock.patch.object(backend.installer.shutil, "copytree", side_effect=OSError("backup-failed-fixture")))
            error = "still running" if stopped_pid else "stop could not be confirmed" if stop_fails else "health-failed" if failure == "health" else "backup-failed"
            with self.assertRaisesRegex((RuntimeError, OSError), error) if failure or stopped_pid else nullcontext():
                backend.installer.apply(metadata, members, "https://fixture.example/health")
        candidate_selected = (failure is None and not stopped_pid) or stop_fails
        self.assertEqual((prefix / "current").resolve(), prefix / "releases" / REVISION if candidate_selected else previous)
        self.assertEqual(unit.read_bytes(), members["mcport.service"][0] if candidate_selected else b"previous-unit-fixture")
        self.assertEqual(service_state[0], "inactive" if stopped_pid or failure == "health" and not stop_fails else "active")
        self.assertEqual(("systemctl", "enable", "mcport.service") in commands, failure is None and not stopped_pid)
        self.assertEqual(removals, [] if stopped_pid else [ipc])
        self.assertEqual(ipc.exists(), bool(stopped_pid))
        for relative, contents in durable.items():
            self.assertEqual((data / relative).read_bytes(), contents)
            self.assertEqual((data / relative).stat().st_mode & 0o777, 0o600)
        self.assertEqual(runtime.stat().st_mode & 0o777, 0o600)
        self.assertEqual(runtime.read_text(), runtime_contents)
        backup = next(backups.iterdir())
        if stopped_pid or failure == "backup":
            self.assertFalse((backup / "runtime.env").exists())
        else:
            for relative, contents in durable.items():
                self.assertEqual((backup / "data" / relative).read_bytes(), contents)
            self.assertFalse((backup / "data" / ipc.relative_to(data)).exists())
            self.assertEqual((backup / "runtime.env").read_bytes(), runtime.read_bytes())

    def test_runtime_data_path_must_be_covered_by_backup(self):
        runtime = self.path / "runtime.env"
        runtime.write_text("MCPORT_DATA_DIR=/elsewhere\nMCPORT_BIND=127.0.0.1:4380\n")
        with self.assertRaisesRegex(ValueError, "MCPORT_DATA_DIR"):
            backend.installer.validate_runtime_locations(runtime)
        runtime.write_text('MCPORT_DATA_DIR="/var/lib/mcport"\nMCPORT_BIND=127.0.0.1:4380\n')
        backend.installer.validate_runtime_locations(runtime)
        with runtime.open("a") as handle:
            handle.write("MCPORT_DATA_DIR=/var/lib/mcport\n")
        with self.assertRaisesRegex(ValueError, "MCPORT_DATA_DIR"):
            backend.installer.validate_runtime_locations(runtime)

    def test_health_requires_exact_source_revision(self):
        response = mock.MagicMock()
        response.__enter__.return_value = response
        response.status = 200
        response.read.return_value = json.dumps({"status": "ok", "version": "0.1.0", "source_revision": "b" * 40}).encode()
        opener = mock.MagicMock()
        opener.open.return_value = response
        with mock.patch.object(backend.installer.urllib.request, "build_opener", return_value=opener), mock.patch.object(backend.installer.time, "sleep"):
            with self.assertRaisesRegex(RuntimeError, "source revision"):
                backend.installer.health("https://fixture.example/health", "0.1.0", REVISION)
            response.read.return_value = json.dumps({"status": "ok", "version": "0.1.0", "source_revision": REVISION}).encode()
            backend.installer.health("https://fixture.example/health", "0.1.0", REVISION)


if __name__ == "__main__":
    unittest.main()
