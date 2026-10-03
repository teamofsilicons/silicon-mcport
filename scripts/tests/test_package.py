"""Header fixtures exercise rejection rules; they are never release artifacts."""
import argparse
import hashlib
import importlib.util
import json
from pathlib import Path
import stat
import struct
import tempfile
import tarfile
import unittest
import zipfile

SPEC = importlib.util.spec_from_file_location("packaging", Path(__file__).parents[1] / "package.py")
package = importlib.util.module_from_spec(SPEC)
SPEC.loader.exec_module(package)


def header_fixture(target):
    data = bytearray(2048)
    arm = target.endswith("aarch64")
    if target.startswith("linux"):
        data[:7] = b"\x7fELF\x02\x01\x01"
        struct.pack_into("<HH", data, 16, 3, 183 if arm else 62)
        struct.pack_into("<H", data, 52, 64)
    elif target.startswith("macos"):
        data[:4] = b"\xcf\xfa\xed\xfe"
        struct.pack_into("<III", data, 4, 0x0100000C if arm else 0x01000007, 0, 2)
    else:
        data[:2] = b"MZ"
        struct.pack_into("<I", data, 0x3C, 128)
        data[128:132] = b"PE\0\0"
        struct.pack_into("<H", data, 132, 0xAA64 if arm else 0x8664)
        struct.pack_into("<H", data, 150, 2)
        struct.pack_into("<H", data, 152, 0x20B)
    return bytes(data)


class PackagingTests(unittest.TestCase):
    def test_six_expected_headers_and_wrong_architecture(self):
        for target in package.TARGETS:
            with self.subTest(target=target):
                data = header_fixture(target)
                package.validate_binary(data, target, 0o755)
                other = target.rsplit("-", 1)[0] + ("-x86_64" if target.endswith("aarch64") else "-aarch64")
                with self.assertRaises(package.PackageError):
                    package.validate_binary(data, other, 0o755)
                with self.assertRaises(package.PackageError):
                    package.validate_binary(data[:32], target, 0o755)
                with self.assertRaises(package.PackageError):
                    package.validate_binary(bytes(2048), target, 0o755)

    def test_wrong_format_nonexecutable_and_missing_binary(self):
        with self.assertRaises(package.PackageError):
            package.validate_binary(header_fixture("macos-x86_64"), "linux-x86_64", 0o755)
        with self.assertRaises(package.PackageError):
            package.validate_binary(header_fixture("linux-x86_64"), "linux-x86_64", 0o644)
        with tempfile.TemporaryDirectory() as directory:
            path = Path(directory) / "missing"
            with self.assertRaises(package.PackageError):
                package.read_binary(path, "linux-x86_64")
            path.write_bytes(b"")
            with self.assertRaises(package.PackageError):
                package.read_binary(path, "linux-x86_64")

    def stage_fixture(self, directory, target, payload=None):
        manifest, _ = package.manifest(package.ROOT / "honeycomb.yaml")
        data = payload if payload is not None else header_fixture(target)
        record = {"target": target, "rust_target": package.TARGETS[target][2], "version": manifest["version"], "sha256": hashlib.sha256(data).hexdigest(), "native_smoke": True}
        package.write_zip(directory / f"mcport-{target}.zip", {package.binary_path(manifest, target): (data, 0o755), f"build/{target}.json": (json.dumps(record).encode(), 0o644)})

    def test_assembly_requires_every_target_and_validates_each(self):
        with tempfile.TemporaryDirectory() as directory:
            path = Path(directory)
            for target in list(package.TARGETS)[:-1]:
                self.stage_fixture(path, target)
            args = argparse.Namespace(manifest=package.ROOT / "honeycomb.yaml", input=path, output=path / "candidate.tar.gz")
            with self.assertRaisesRegex(package.PackageError, "Missing native targets"):
                package.assemble(args)
            self.assertFalse(args.output.exists())
            self.stage_fixture(path, "windows-aarch64", header_fixture("windows-x86_64"))
            with self.assertRaisesRegex(package.PackageError, "wrong PE architecture"):
                package.assemble(args)
            self.assertFalse(args.output.exists())

    def test_deterministic_complete_tar_and_no_overwrite(self):
        with tempfile.TemporaryDirectory() as directory:
            path = Path(directory)
            staged = path / "staged"
            staged.mkdir()
            for target in package.TARGETS:
                self.stage_fixture(staged, target)
            first, second = path / "first.tar.gz", path / "second.tar.gz"
            for output in (first, second):
                package.assemble(argparse.Namespace(manifest=package.ROOT / "honeycomb.yaml", input=staged, output=output))
            self.assertEqual(first.read_bytes(), second.read_bytes())
            with tarfile.open(first) as archive:
                self.assertEqual(len(archive.getmembers()), 7)
                for target in package.TARGETS:
                    data, _ = package.manifest(package.ROOT / "honeycomb.yaml")
                    info = archive.getmember(package.binary_path(data, target))
                    self.assertEqual(info.mode, 0o755)
                    self.assertEqual(info.mtime, 0)
                    self.assertTrue(info.isfile())
            before = first.read_bytes()
            with self.assertRaises(package.PackageError):
                package.write_honeycomb_tar(first, {})
            self.assertEqual(before, first.read_bytes())

    def test_duplicate_manifest_keys_rejected(self):
        with tempfile.TemporaryDirectory() as directory:
            path = Path(directory) / "honeycomb.yaml"
            path.write_text("app_id: mcport\napp_id: other\n")
            with self.assertRaisesRegex(package.PackageError, "Duplicate manifest"):
                package.manifest(path)

    def test_zip_path_injection_and_mutated_checksum_rejected(self):
        with tempfile.TemporaryDirectory() as directory:
            path = Path(directory)
            target = "linux-x86_64"
            manifest, _ = package.manifest(package.ROOT / "honeycomb.yaml")
            data = header_fixture(target)
            record = {"target": target, "rust_target": package.TARGETS[target][2], "version": manifest["version"], "sha256": "0" * 64, "native_smoke": True}
            filename = path / "mcport-linux-x86_64.zip"
            args = argparse.Namespace(manifest=package.ROOT / "honeycomb.yaml", input=path, output=path / "candidate.tar.gz")
            package.write_zip(filename, {"../mcport": (data, 0o755), f"build/{target}.json": (json.dumps(record).encode(), 0o644)})
            with self.assertRaisesRegex(package.PackageError, "Unexpected native archive paths"):
                package.assemble(args)
            filename.unlink()
            package.write_zip(filename, {package.binary_path(manifest, target): (data, 0o755), f"build/{target}.json": (json.dumps(record).encode(), 0o644)})
            with self.assertRaisesRegex(package.PackageError, "checksum mismatch"):
                package.assemble(args)


if __name__ == "__main__":
    unittest.main()
