#!/usr/bin/env python3
"""Exercise installs and rejected updates entirely in temporary directories."""
import hashlib
import io
import os
from pathlib import Path
import platform
import subprocess
import tarfile
import tempfile
import unittest

INSTALLER = Path(__file__).resolve().with_name("install.sh")


class InstallTests(unittest.TestCase):
    def setUp(self):
        self.temp = tempfile.TemporaryDirectory(prefix="empire-install-test-")
        self.addCleanup(self.temp.cleanup)
        self.root = Path(self.temp.name)
        self.assets = self.root / "assets"
        self.assets.mkdir()
        self.dest = self.root / "bin with spaces"
        self.archive = self.assets / f"empire-linux-{platform.machine()}.tar.gz"
        with tarfile.open(self.archive, "w:gz") as tar:
            for name, data in [("empire", b'#!/bin/sh\nprintf "empire 0.1.0\\n"\n'), ("VERSION", b"0.1.0\n"),
                               ("LICENSE", b"Empire license\n"), ("THIRD-PARTY-NOTICES.txt", b"Dependency attribution\n"),
                               ("RUST-THIRD-PARTY-NOTICES.html", b"Rust attribution\n")]:
                info = tarfile.TarInfo(name)
                info.size = len(data)
                info.mode = 0o755 if name == "empire" else 0o644
                tar.addfile(info, io.BytesIO(data))
        self.checksum = self.assets / "SHA256SUMS"
        self.checksum.write_text(f"{hashlib.sha256(self.archive.read_bytes()).hexdigest()}  {self.archive.name}\n")

    def install(self, *args, success=True):
        result = subprocess.run(["bash", str(INSTALLER), "--from-dir", str(self.assets), "--bin-dir", str(self.dest), *args], text=True, capture_output=True)
        self.assertEqual(result.returncode == 0, success, result.stdout + result.stderr)
        return result

    def existing(self):
        self.dest.mkdir()
        binary = self.dest / "empire"
        binary.write_text("existing installation")
        return binary

    def test_install_version_and_permissions(self):
        self.install("--version", "v0.1.0")
        binary = self.dest / "empire"
        self.assertEqual(subprocess.check_output([binary, "--version"], text=True).strip(), "empire 0.1.0")
        self.assertEqual(binary.stat().st_mode & 0o777, 0o755)
        self.assertFalse(list(self.dest.glob(".empire-install.*")))
        self.assertEqual((self.dest / "empire.THIRD-PARTY-NOTICES.txt").read_text(), "Dependency attribution\n")
        self.assertEqual((self.dest / "empire.RUST-THIRD-PARTY-NOTICES.html").read_text(), "Rust attribution\n")

    def test_replace_symlink_without_writing_to_checkout(self):
        self.dest.mkdir()
        checkout = self.root / "checkout-binary"
        checkout.write_text("keep this build")
        (self.dest / "empire").symlink_to(checkout)
        self.install()
        self.assertFalse((self.dest / "empire").is_symlink())
        self.assertEqual(checkout.read_text(), "keep this build")

    def test_corrupt_archive_preserves_installed_binary(self):
        binary = self.existing()
        with self.archive.open("ab") as f:
            f.write(b"corruption")
        self.install(success=False)
        self.assertEqual(binary.read_text(), "existing installation")

    def test_wrong_version_preserves_installed_binary(self):
        binary = self.existing()
        self.install("--version", "v0.2.0", success=False)
        self.assertEqual(binary.read_text(), "existing installation")

    def test_missing_and_ambiguous_checksums(self):
        valid = self.checksum.read_text()
        for manifest in ["", valid + valid, valid.replace(self.archive.name, "../../elsewhere")]:
            self.checksum.write_text(manifest)
            self.install(success=False)
            self.assertFalse(self.dest.exists())

    def test_invalid_options_and_versions(self):
        for args in [("--version", "../../other"), ("--bin-dir",), ("--unknown",)]:
            self.install(*args, success=False)
        self.assertFalse(self.dest.exists())


if __name__ == "__main__":
    unittest.main()
