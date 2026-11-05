"""Unit tests for scripts/release.py (standard library only, no network)."""

from __future__ import annotations

import gzip
import io
import json
import shutil
import struct
import subprocess
import sys
import tarfile
import tempfile
import unittest
import unittest.mock
from pathlib import Path

sys.path.insert(0, str(Path(__file__).resolve().parent.parent))
import release  # noqa: E402

LINUX = release.target_by_id("linux-x64-gnu")
WINDOWS = release.target_by_id("win32-x64")
DARWIN = release.target_by_id("darwin-arm64")
VERSION = release.workspace_version()


def elf(machine: int = 62, glibc: tuple = ("GLIBC_2.17", "GLIBC_2.34", "GLIBC_PRIVATE")) -> bytes:
    """A minimal ELF64 image with a .gnu.version_r section requiring `glibc`."""
    strings = b"\0libc.so.6\0" + b"".join(name.encode() + b"\0" for name in glibc)
    offsets = [strings.index(name.encode() + b"\0") for name in glibc]
    verneed = struct.pack("<HHIII", 1, len(glibc), 1, 16, 0)
    for i, offset in enumerate(offsets):
        verneed += struct.pack("<IHHII", 0, 0, 2 + i, offset, 16 if i + 1 < len(glibc) else 0)
    header = bytearray(64)
    header[:6] = b"\x7fELF\x02\x01"
    struct.pack_into("<H", header, 18, machine)
    strings_at = 64
    verneed_at = strings_at + len(strings)
    sections_at = verneed_at + len(verneed)
    struct.pack_into("<Q", header, 0x28, sections_at)
    struct.pack_into("<HH", header, 0x3A, 64, 3)
    sections = struct.pack("<IIQQQQIIQQ", *([0] * 10))
    sections += struct.pack("<IIQQQQIIQQ", 0, 3, 0, 0, strings_at, len(strings), 0, 0, 1, 0)
    sections += struct.pack("<IIQQQQIIQQ", 0, 0x6FFFFFFE, 0, 0, verneed_at, len(verneed), 1, 1, 8, 0)
    return bytes(header) + strings + verneed + sections


def pe(machine: int = 0x8664) -> bytes:
    data = bytearray(256)
    data[:2] = b"MZ"
    struct.pack_into("<I", data, 60, 128)
    data[128:132] = b"PE\0\0"
    struct.pack_into("<H", data, 132, machine)
    return bytes(data)


def tar_gz(members: list) -> bytes:
    raw = io.BytesIO()
    with tarfile.open(fileobj=raw, mode="w:gz", format=tarfile.USTAR_FORMAT) as tar:
        for name, data, kind in members:
            info = tarfile.TarInfo(name)
            info.type, info.mode, info.size = kind, 0o755, len(data)
            if kind == tarfile.SYMTYPE:
                info.linkname, info.size = "elsewhere", 0
            tar.addfile(info, io.BytesIO(data) if kind == tarfile.REGTYPE else None)
    return raw.getvalue()


class ArchiveTests(unittest.TestCase):
    def test_pack_is_deterministic_with_sorted_fixed_metadata(self):
        entries = {"b": b"two", "a": b"one"}
        packed = release.pack(entries)
        self.assertEqual(packed, release.pack(dict(reversed(list(entries.items())))))
        self.assertEqual(packed[4:8], b"\0\0\0\0", "gzip header has no timestamp")
        with tarfile.open(fileobj=io.BytesIO(packed), mode="r:gz") as tar:
            members = tar.getmembers()
        self.assertEqual([m.name for m in members], ["a", "b"])
        for member in members:
            self.assertEqual((member.mtime, member.uid, member.gid, member.uname, member.gname, member.mode),
                             (0, 0, 0, "", "", 0o755))
            self.assertTrue(member.isreg())

    def test_archive_holds_exactly_the_executable(self):
        packed = release.pack({"dsd.exe": b"binary"})
        self.assertEqual(release.archive_executable(packed, WINDOWS), b"binary")
        invalid = [
            release.pack({"dsd": b"binary"}),
            release.pack({"dsd.exe": b"binary", "README.md": b"readme"}),
            release.pack({"dsd.exe": b""}),
            tar_gz([("dsd.exe", b"", tarfile.SYMTYPE)]),
            tar_gz([("bin/dsd.exe", b"binary", tarfile.REGTYPE)]),
            b"not gzip",
        ]
        for data in invalid:
            with self.assertRaises(release.ReleaseError):
                release.archive_executable(data, WINDOWS)


class BinaryTests(unittest.TestCase):
    def test_formats_and_architectures_are_checked(self):
        self.assertEqual(release.verify_binary(elf(), LINUX), "2.34")
        self.assertIsNone(release.verify_binary(pe(), WINDOWS))
        macho = struct.pack("<II", 0xFEEDFACF, 0x0100000C) + bytes(32)
        self.assertIsNone(release.verify_binary(macho, DARWIN))
        for data, target in [(elf(machine=183), LINUX), (pe(machine=0x14C), WINDOWS), (pe(), LINUX),
                             (elf(), DARWIN), (struct.pack("<II", 0xFEEDFACF, 7) + bytes(32), DARWIN)]:
            with self.assertRaises(release.ReleaseError, msg=target["id"]):
                release.verify_binary(data, target)

    def test_glibc_floor_is_measured_and_bounded(self):
        self.assertEqual(release.max_glibc(elf(glibc=("GLIBC_2.2.5", "GLIBC_2.28"))), (2, 28))
        self.assertIsNone(release.max_glibc(elf(glibc=("GLIBC_PRIVATE",))))
        with self.assertRaisesRegex(release.ReleaseError, "glibc 2.40"):
            release.verify_binary(elf(glibc=("GLIBC_2.40",)), LINUX)

    def test_fixture_builds_are_refused(self):
        with self.assertRaisesRegex(release.ReleaseError, "fixture"):
            release.verify_binary(pe() + b"DSD_FIXTURE_RELEASE_ORIGIN", WINDOWS)


class VersionTests(unittest.TestCase):
    def copy_root(self, tmp: Path) -> Path:
        for name in ("Cargo.toml", "Cargo.lock"):
            shutil.copyfile(release.ROOT / name, tmp / name)
        for crate in ("dsd", "dsd-cli"):
            (tmp / "crates" / crate).mkdir(parents=True)
            shutil.copyfile(release.ROOT / "crates" / crate / "Cargo.toml", tmp / "crates" / crate / "Cargo.toml")
        return tmp

    def test_tag_must_be_stable_and_match_cargo(self):
        release.check_tag(f"v{VERSION}", VERSION)
        for tag in [VERSION, f"v{VERSION}-rc.1", "v0.01.0", "v9.9.9"]:
            with self.assertRaises(release.ReleaseError, msg=tag):
                release.check_tag(tag, VERSION)

    def test_lockfile_must_agree_with_the_workspace_version(self):
        with tempfile.TemporaryDirectory() as tmp:
            root = self.copy_root(Path(tmp))
            self.assertEqual(release.workspace_version(root), VERSION)
            manifest = root / "Cargo.toml"
            manifest.write_text(manifest.read_text(encoding="utf-8").replace(
                f'version = "{VERSION}"', 'version = "9.9.9"', 1), encoding="utf-8")
            with self.assertRaisesRegex(release.ReleaseError, "Cargo.lock"):
                release.workspace_version(root)


class InventoryTests(unittest.TestCase):
    def test_every_target_has_a_native_runner_and_one_executable_name(self):
        ids = [t["id"] for t in release.TARGETS]
        self.assertEqual(ids, ["linux-x64-gnu", "linux-arm64-gnu", "darwin-arm64", "win32-x64"])
        for target in release.TARGETS:
            self.assertTrue(target["runner"])
            self.assertEqual(target["executable"], "dsd.exe" if target["os"] == "win32" else "dsd")
            self.assertEqual("maxGlibc" in target, target["os"] == "linux")

    def test_matrix_and_installers_derive_from_the_inventory(self):
        out = io.StringIO()
        with unittest.mock.patch("sys.stdout", out):
            self.assertEqual(release.main(["matrix"]), 0)
        self.assertEqual([e["id"] for e in json.loads(out.getvalue())["include"]], [t["id"] for t in release.TARGETS])
        rendered = release.render_installers("1.2.3")
        self.assertIn("  Linux:aarch64|Linux:arm64) target=linux-arm64-gnu ;;", rendered["install.sh"])
        self.assertIn("  Darwin:arm64) target=darwin-arm64 ;;", rendered["install.sh"])
        self.assertIn('archive_name="dsd-1.2.3-$target.tar.gz"', rendered["install.sh"])
        self.assertIn("'dsd-1.2.3-win32-x64.tar.gz'", rendered["install.ps1"])
        self.assertIn("target = 'win32-x64'", rendered["install.ps1"])


def fake_build(archives: Path, target: dict, executable: bytes, revision: str) -> dict:
    packed = release.pack({target["executable"]: executable})
    name = release.archive_name(VERSION, target)
    (archives / name).write_bytes(packed)
    record = {"target": target["id"], "version": VERSION, "sourceRevision": revision, "compiler": "rustc 1.88.0",
              "archive": name, "sha256": release.digest(packed), "executableSha256": release.digest(executable),
              "glibc": None}
    release.write_json(archives / f"{target['id']}.json", record)
    return record


class BundleFixture(unittest.TestCase):
    """Fake build records and certification reports for every target."""

    def setUp(self):
        quiet = unittest.mock.patch("sys.stdout", new_callable=io.StringIO)
        quiet.start()
        self.addCleanup(quiet.stop)
        self.tmp = Path(tempfile.mkdtemp(prefix="dsd-release-test-"))
        self.revision = release.source_revision()
        self.archives, self.reports, self.bundle = (self.tmp / n for n in ("archives", "reports", "bundle"))
        self.archives.mkdir()
        self.reports.mkdir()
        for target in release.TARGETS:
            record = fake_build(self.archives, target, f"binary for {target['id']}".encode(), self.revision)
            report = dict(record, certified=True, os=target["os"], arch=target["arch"], checks=["installer"])
            release.write_json(self.reports / f"{target['id']}.json", report)

    def tearDown(self):
        shutil.rmtree(self.tmp)


class BundleTests(BundleFixture):
    def test_assembled_bundle_lists_every_asset_with_checksums(self):
        manifest = release.assemble(self.archives, self.reports, self.bundle)
        self.assertEqual(manifest["tag"], f"v{VERSION}")
        self.assertEqual([e["target"] for e in manifest["targets"]], [t["id"] for t in release.TARGETS])
        self.assertTrue(all(e["runtimeCertified"] for e in manifest["targets"]))
        sums = (self.bundle / "SHA256SUMS").read_text(encoding="utf-8").splitlines()
        self.assertEqual([line.split("  ")[1] for line in sums], release.bundle_files(VERSION))
        assets = release.verify_bundle(self.bundle, self.revision)
        self.assertIn("verify-windows-install.ps1", assets)
        self.assertIn("release-manifest.json", assets)

    def test_assembly_requires_every_certification(self):
        (self.reports / "darwin-arm64.json").unlink()
        with self.assertRaises(FileNotFoundError):
            release.assemble(self.archives, self.reports, self.bundle)
        report = json.loads((self.reports / "win32-x64.json").read_text(encoding="utf-8"))
        release.write_json(self.reports / "darwin-arm64.json", report)
        with self.assertRaisesRegex(release.ReleaseError, "darwin-arm64"):
            release.assemble(self.archives, self.reports, self.tmp / "other")

    def test_tampered_bundles_are_rejected(self):
        release.assemble(self.archives, self.reports, self.bundle)
        (self.bundle / "extra.txt").write_text("x", encoding="utf-8")
        with self.assertRaisesRegex(release.ReleaseError, "unexpected"):
            release.verify_bundle(self.bundle, self.revision)
        (self.bundle / "extra.txt").unlink()
        sums = self.bundle / "SHA256SUMS"
        sums.write_text(sums.read_text(encoding="utf-8").replace("install.sh", "install.sh "), encoding="utf-8")
        with self.assertRaisesRegex(release.ReleaseError, "SHA256SUMS"):
            release.verify_bundle(self.bundle, self.revision)
        with self.assertRaisesRegex(release.ReleaseError, "source"):
            release.verify_bundle(self.bundle, "0" * 40)


class FakeGh:
    """Records gh calls and simulates one GitHub Release."""

    def __init__(self, bundle: Path, sha: str, tag: str):
        self.bundle, self.sha, self.tag = bundle, sha, tag
        self.release = None
        self.assets: dict = {}
        self.calls: list = []

    def __call__(self, args):
        self.calls.append(list(args))
        ok = lambda out="": subprocess.CompletedProcess(args, 0, out, "")  # noqa: E731
        if args[0] == "api":
            return ok(self.sha + "\n")
        command = args[1]
        if command == "view":
            if self.release is None:
                return subprocess.CompletedProcess(args, 1, "", "release not found\n")
            return ok(json.dumps(dict(self.release, assets=[{"name": n} for n in sorted(self.assets)])))
        if command == "create":
            self.release = {"tagName": self.tag, "isDraft": True, "isPrerelease": False, "name": self.tag,
                            "body": "notes", "url": f"https://github.com/cpaikr/dsdai/releases/tag/{self.tag}"}
            return ok()
        if command == "upload":
            for path in args[5:]:
                self.assets[Path(path).name] = Path(path).read_bytes()
            return ok()
        if command == "download":
            target = Path(args[args.index("--dir") + 1])
            for name, data in self.assets.items():
                (target / name).write_bytes(data)
            return ok()
        if command == "edit":
            self.release["isDraft"] = False
            return ok()
        raise AssertionError(args)


class PublishTests(BundleFixture):
    def test_publication_creates_uploads_and_publishes_once(self):
        release.assemble(self.archives, self.reports, self.bundle)
        tag = f"v{VERSION}"
        gh = FakeGh(self.bundle, self.revision, tag)
        self.assertEqual(release.publish(release.REPOSITORY, tag, self.revision, self.bundle, gh), "published")
        self.assertFalse(gh.release["isDraft"])
        self.assertEqual(sorted(gh.assets), sorted(release.verify_bundle(self.bundle, self.revision)))
        # A rerun verifies the published release without changing it.
        uploads = [c for c in gh.calls if c[:2] == ["release", "upload"]]
        self.assertEqual(len(uploads), 1)
        self.assertEqual(release.publish(release.REPOSITORY, tag, self.revision, self.bundle, gh), "verified-or-resumed")
        self.assertEqual(uploads, [c for c in gh.calls if c[:2] == ["release", "upload"]])

    def test_differing_remote_assets_stop_publication(self):
        release.assemble(self.archives, self.reports, self.bundle)
        tag = f"v{VERSION}"
        gh = FakeGh(self.bundle, self.revision, tag)
        gh("release create".split())
        gh.assets["install.sh"] = b"something else"
        with self.assertRaisesRegex(release.ReleaseError, "differs"):
            release.publish(release.REPOSITORY, tag, self.revision, self.bundle, gh)
        self.assertTrue(gh.release["isDraft"])

    def test_a_moved_tag_stops_publication(self):
        release.assemble(self.archives, self.reports, self.bundle)
        gh = FakeGh(self.bundle, "f" * 40, f"v{VERSION}")
        with self.assertRaisesRegex(release.ReleaseError, "remote tag"):
            release.publish(release.REPOSITORY, f"v{VERSION}", self.revision, self.bundle, gh)
        self.assertIsNone(gh.release)


if __name__ == "__main__":
    unittest.main()
