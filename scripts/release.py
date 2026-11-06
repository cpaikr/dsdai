#!/usr/bin/env python3
"""Build, certify, assemble, and publish `dsd` standalone releases.

Standard library only. `scripts/release-targets.json` is the one target
inventory: CI matrices, archive names, installer platform cases, and the
release manifest all derive from it.

Usage:
  python scripts/release.py matrix
  python scripts/release.py version [--tag vX.Y.Z]
  python scripts/release.py build TARGET OUT_DIR [--executable PATH]
  python scripts/release.py verify-archive TARGET ARCHIVE
  python scripts/release.py certify TARGET ARCHIVE_DIR REPORT
  python scripts/release.py assemble ARCHIVE_DIR REPORT_DIR BUNDLE_DIR
  python scripts/release.py verify-bundle BUNDLE_DIR [--sha SHA]
  python scripts/release.py publish --repository OWNER/REPO --tag vX.Y.Z --sha SHA --directory BUNDLE_DIR

`build` compiles the production executable (no fixture-origin feature) for
the target unless --executable names a prebuilt one, checks its binary format,
and writes `dsd-<version>-<target>.tar.gz` plus a `<target>.json` build
record. `certify` must run on the target platform: it installs the archive
with the release installer into a clean temporary directory and checks the
installed executable, its receipt, and failed-checksum recovery. `assemble`
requires every target's build record and certification report, then writes
SHA256SUMS and release-manifest.json. `publish` creates or resumes the GitHub
Release through `gh` and never replaces existing assets.
"""

from __future__ import annotations

import argparse
import gzip
import hashlib
import io
import json
import os
import platform
import re
import shutil
import struct
import subprocess
import sys
import tarfile
import tempfile
from pathlib import Path
from typing import Callable, Dict, List, Optional, Sequence

ROOT = Path(__file__).resolve().parent.parent
LICENSE = "LICENSE.md"
TARGETS: List[dict] = json.loads((ROOT / "scripts" / "release-targets.json").read_text(encoding="utf-8"))
REPOSITORY = "cpaikr/dsdai"
INSTALLERS = ("install.sh", "install.ps1")
# Shipped verbatim beside the installers; not part of the archive.
VERIFY_SCRIPT = "verify-windows-install.ps1"
RECEIPT = ".dsd-receipt.json"
STABLE_TAG = re.compile(r"^v(0|[1-9]\d*)\.(0|[1-9]\d*)\.(0|[1-9]\d*)$")
# Child output is UTF-8 (dsd, gh); never decode it with the Windows ANSI code page.
TEXT = {"text": True, "encoding": "utf-8", "errors": "replace"}
# Release executables must not contain the test-only fixture-origin hooks.
FIXTURE_TOKENS = (b"DSD_FIXTURE_RELEASE_ORIGIN", b"DSD_FIXTURE_RUNNING_VERSION")


class ReleaseError(Exception):
    """A release check failed; the message says which and why."""


def require(condition: bool, message: str) -> None:
    if not condition:
        raise ReleaseError(message)


def digest(data: bytes) -> str:
    return hashlib.sha256(data).hexdigest()


def file_digest(path: Path) -> str:
    return digest(path.read_bytes())


def target_by_id(target_id: str) -> dict:
    for target in TARGETS:
        if target["id"] == target_id:
            return target
    raise ReleaseError(f"Unknown release target {target_id!r}; see scripts/release-targets.json.")


def run(args: Sequence[str], **kwargs) -> str:
    proc = subprocess.run(list(args), cwd=kwargs.pop("cwd", ROOT), capture_output=True, **TEXT, **kwargs)
    if proc.returncode != 0:
        raise ReleaseError(f"{args[0]} {' '.join(args[1:3])} exited {proc.returncode}: {(proc.stderr or proc.stdout).strip()}")
    return proc.stdout


# ---------------------------------------------------------------------------
# version authority


def workspace_version(root: Path = ROOT) -> str:
    """`[workspace.package] version` from Cargo.toml, checked against Cargo.lock."""
    manifest = (root / "Cargo.toml").read_text(encoding="utf-8")
    section = re.search(r"^\[workspace\.package\]\s*$(.*?)(?=^\[|\Z)", manifest, re.M | re.S)
    require(section is not None, "Cargo.toml has no [workspace.package] section.")
    found = re.search(r'^version\s*=\s*"([^"]+)"\s*$', section.group(1), re.M)
    require(found is not None, "[workspace.package] has no version.")
    version = found.group(1)
    require(STABLE_TAG.match(f"v{version}") is not None, f"Workspace version {version} is not a plain x.y.z version.")
    for crate in ("dsd", "dsd-cli"):
        text = (root / "crates" / crate / "Cargo.toml").read_text(encoding="utf-8")
        require(re.search(r"^version\.workspace\s*=\s*true\s*$", text, re.M) is not None,
                f"crates/{crate} must inherit the workspace version.")
    lock = (root / "Cargo.lock").read_text(encoding="utf-8")
    for crate in ("dsd", "dsd-cli"):
        entry = re.search(rf'^name = "{re.escape(crate)}"\nversion = "([^"]+)"$', lock, re.M)
        require(entry is not None and entry.group(1) == version,
                f"Cargo.lock lists {crate} {entry.group(1) if entry else 'nowhere'}, not {version}; run cargo update -w.")
    return version


def check_tag(tag: str, version: str) -> None:
    require(STABLE_TAG.match(tag) is not None, f"Tag {tag} is not a stable vX.Y.Z tag.")
    require(tag == f"v{version}", f"Tag {tag} does not match the Cargo workspace version v{version}.")


def archive_name(version: str, target: dict) -> str:
    return f"dsd-{version}-{target['id']}.tar.gz"


# ---------------------------------------------------------------------------
# archives


def pack(entries: Dict[str, bytes]) -> bytes:
    """A deterministic tar.gz: sorted names, mode 0644 for the license and 0755
    otherwise, zero mtime/uid/gid, empty owner names, USTAR, and a gzip header
    without name or time."""
    raw = io.BytesIO()
    with tarfile.open(fileobj=raw, mode="w", format=tarfile.USTAR_FORMAT) as tar:
        for name in sorted(entries):
            info = tarfile.TarInfo(name)
            info.size = len(entries[name])
            info.mode = 0o644 if name == LICENSE else 0o755
            info.mtime = 0
            info.uid = info.gid = 0
            info.uname = info.gname = ""
            info.type = tarfile.REGTYPE
            tar.addfile(info, io.BytesIO(entries[name]))
    out = io.BytesIO()
    with gzip.GzipFile(filename="", fileobj=out, mode="wb", compresslevel=9, mtime=0) as gz:
        gz.write(raw.getvalue())
    return out.getvalue()


def archive_executable(data: bytes, target: dict) -> bytes:
    """The executable from an archive holding exactly it and the license as
    regular files, the same rule `dsd upgrade` and the installers enforce."""
    try:
        with tarfile.open(fileobj=io.BytesIO(data), mode="r:gz") as tar:
            members = tar.getmembers()
            expected = sorted([target["executable"], LICENSE])
            require([m.name for m in members] == expected,
                    f"Archive must contain exactly {expected}, not {[m.name for m in members]}.")
            license_member = next(m for m in members if m.name == LICENSE)
            require(license_member.isreg() and license_member.mode & 0o777 == 0o644,
                    "The archived license must be a regular file with mode 0644.")
            member = next(m for m in members if m.name == target["executable"])
            require(member.isreg() and member.size > 0, "The archived executable must be a non-empty regular file.")
            require(member.mode & 0o777 == 0o755, "The archived executable must have mode 0755.")
            handle = tar.extractfile(member)
            assert handle is not None
            return handle.read()
    except (tarfile.TarError, OSError, EOFError) as error:
        raise ReleaseError(f"Archive is not a readable tar.gz: {error}") from None


def max_glibc(data: bytes) -> Optional[tuple]:
    """Highest GLIBC_x.y symbol version an ELF64 executable requires, from its
    .gnu.version_r section; None when it requires no versioned glibc symbol."""
    require(data[:4] == b"\x7fELF" and data[4] == 2 and data[5] == 1, "Expected a little-endian ELF64 executable.")
    shoff, = struct.unpack_from("<Q", data, 0x28)
    shentsize, shnum = struct.unpack_from("<HH", data, 0x3A)
    sections = [struct.unpack_from("<IIQQQQIIQQ", data, shoff + i * shentsize) for i in range(shnum)]
    found = []
    for section in sections:
        if section[1] != 0x6FFFFFFE:  # SHT_GNU_verneed
            continue
        strings = sections[section[6]][4]
        offset, count = section[4], section[7]
        for _ in range(count):
            _, aux_count, _, aux, following = struct.unpack_from("<HHIII", data, offset)
            position = offset + aux
            for _ in range(aux_count):
                _, _, _, name, aux_next = struct.unpack_from("<IHHII", data, position)
                end = data.index(b"\0", strings + name)
                version = re.fullmatch(rb"GLIBC_(\d+)\.(\d+)(?:\.(\d+))?", data[strings + name:end])
                if version:
                    found.append(tuple(int(part) for part in version.groups() if part is not None))
                position += aux_next
            offset += following
    return max(found) if found else None


def verify_binary(data: bytes, target: dict) -> Optional[str]:
    """Checks the executable format and architecture; returns the glibc floor for Linux."""
    for token in FIXTURE_TOKENS:
        require(token not in data, "Refusing an executable built with the fixture-origin test feature.")
    if target["os"] == "linux":
        require(data[:4] == b"\x7fELF" and data[4] == 2, "Expected a 64-bit ELF executable.")
        machine, = struct.unpack_from("<H", data, 18)
        require(machine == (62 if target["arch"] == "x64" else 183), "ELF architecture does not match the target.")
        floor = max_glibc(data)
        if floor is None:
            return None
        limit = tuple(int(part) for part in target["maxGlibc"].split("."))
        text = ".".join(str(part) for part in floor)
        require(floor <= limit, f"The executable requires glibc {text}, above the declared {target['maxGlibc']}.")
        return text
    if target["os"] == "darwin":
        magic, cpu = struct.unpack_from("<II", data, 0)
        require(magic == 0xFEEDFACF, "Expected a 64-bit Mach-O executable.")
        require(cpu == 0x0100000C, "Mach-O architecture does not match the target.")
        return None
    require(data[:2] == b"MZ", "Expected a Windows executable.")
    pe, = struct.unpack_from("<I", data, 60)
    require(data[pe:pe + 4] == b"PE\0\0", "Expected a PE signature.")
    machine, = struct.unpack_from("<H", data, pe + 4)
    require(machine == 0x8664, "PE architecture does not match the target.")
    return None


# ---------------------------------------------------------------------------
# build


def source_revision() -> str:
    return run(["git", "rev-parse", "HEAD"]).strip()


def compiler() -> str:
    return run(["rustc", "--version"]).strip()


def build(target_id: str, out: Path, executable: Optional[Path]) -> dict:
    target = target_by_id(target_id)
    version = workspace_version()
    if executable is None:
        env = dict(os.environ)
        if target["os"] == "win32":
            # A static C runtime spares consumers a Visual C++ redistributable.
            env["CARGO_TARGET_X86_64_PC_WINDOWS_MSVC_RUSTFLAGS"] = "-C target-feature=+crt-static"
        proc = subprocess.run(
            ["cargo", "build", "--release", "--locked", "-p", "dsd-cli", "--target", target["rustTarget"]],
            cwd=ROOT, env=env)
        require(proc.returncode == 0, "cargo build failed.")
        executable = ROOT / "target" / target["rustTarget"] / "release" / target["executable"]
    data = executable.read_bytes()
    glibc = verify_binary(data, target)
    packed = pack({target["executable"]: data, LICENSE: (ROOT / LICENSE).read_bytes()})
    out.mkdir(parents=True, exist_ok=True)
    name = archive_name(version, target)
    (out / name).write_bytes(packed)
    record = {
        "target": target["id"], "version": version, "sourceRevision": source_revision(), "compiler": compiler(),
        "archive": name, "sha256": digest(packed), "executableSha256": digest(data), "glibc": glibc,
    }
    write_json(out / f"{target['id']}.json", record)
    print(f"built {name} {record['sha256']}")
    return record


def write_json(path: Path, value: object) -> None:
    path.write_text(json.dumps(value, indent=2) + "\n", encoding="utf-8", newline="\n")


def read_build(directory: Path, target: dict, version: str, revision: str) -> dict:
    record = json.loads((directory / f"{target['id']}.json").read_text(encoding="utf-8"))
    require(record["target"] == target["id"], f"{target['id']}: build record names {record['target']}.")
    require(record["version"] == version, f"{target['id']}: built version {record['version']}, expected {version}.")
    require(record["sourceRevision"] == revision, f"{target['id']}: archive source differs from this checkout.")
    require(record["archive"] == archive_name(version, target), f"{target['id']}: unexpected archive name.")
    archive = (directory / record["archive"]).read_bytes()
    require(record["sha256"] == digest(archive), f"{target['id']}: archive checksum mismatch.")
    require(record["executableSha256"] == digest(archive_executable(archive, target)),
            f"{target['id']}: archived executable differs from its build record.")
    return record


# ---------------------------------------------------------------------------
# installers


def render_installers(version: str) -> Dict[str, str]:
    """install.sh and install.ps1 for `version`, with platform cases from the inventory."""
    unix = []
    for target in TARGETS:
        if target["os"] == "win32":
            continue
        system = {"linux": "Linux", "darwin": "Darwin"}[target["os"]]
        machines = {("linux", "x64"): ["x86_64"], ("linux", "arm64"): ["aarch64", "arm64"],
                    ("darwin", "arm64"): ["arm64"]}[(target["os"], target["arch"])]
        pattern = "|".join(f"{system}:{machine}" for machine in machines)
        unix.append(f"  {pattern}) target={target['id']} ;;")
    windows = [target for target in TARGETS if target["os"] == "win32"]
    require(len(windows) == 1, "install.ps1 expects exactly one Windows target.")
    rendered = {}
    for name in INSTALLERS:
        text = (ROOT / "scripts" / "install" / name).read_text(encoding="utf-8")
        text = (text.replace("@@VERSION@@", version).replace("@@UNIX_TARGETS@@", "\n".join(unix))
                .replace("@@WINDOWS_ARCHIVE@@", archive_name(version, windows[0]))
                .replace("@@WINDOWS_TARGET@@", windows[0]["id"]))
        require("@@" not in text, f"{name} has an unrendered placeholder.")
        rendered[name] = text
    return rendered


def host_target() -> Optional[dict]:
    system = {"Linux": "linux", "Darwin": "darwin", "Windows": "win32"}.get(platform.system())
    arch = {"x86_64": "x64", "AMD64": "x64", "aarch64": "arm64", "arm64": "arm64", "ARM64": "arm64"}.get(platform.machine())
    return next((t for t in TARGETS if t["os"] == system and t["arch"] == arch), None)


def installer_command(target: dict, installer_dir: Path, archive: Path, checksums: Path, bin_dir: Path) -> List[str]:
    if target["os"] == "win32":
        # The inbox Windows PowerShell is what a clean consumer has.
        shell = shutil.which("powershell") or "powershell"
        return [shell, "-NoProfile", "-ExecutionPolicy", "Bypass", "-File", str(installer_dir / "install.ps1"),
                "-Archive", str(archive), "-Checksums", str(checksums), "-BinDirectory", str(bin_dir)]
    return ["sh", str(installer_dir / "install.sh"), str(archive), str(checksums), str(bin_dir)]


def expected_receipt(version: str, target: dict, record: dict, executable: str) -> dict:
    return {
        "schemaVersion": 1, "manager": "standalone", "version": version, "target": target["id"],
        "executable": executable, "releaseRepository": REPOSITORY, "releaseTag": f"v{version}",
        "assetName": record["archive"], "sha256": record["executableSha256"],
    }


def same_path(left: str, right: str) -> bool:
    return os.path.normcase(os.path.realpath(left)) == os.path.normcase(os.path.realpath(right))


def certify(target_id: str, directory: Path, report: Path) -> dict:
    target = target_by_id(target_id)
    require(host_target() == target, f"Certification of {target_id} must run on that platform.")
    version = workspace_version()
    record = read_build(directory, target, version, source_revision())
    archive = (directory / record["archive"]).resolve()
    checks = []
    with tempfile.TemporaryDirectory(prefix="dsd-consumer-") as tmp:
        consumer = Path(tmp)
        for name, text in render_installers(version).items():
            (consumer / name).write_text(text, encoding="utf-8", newline="\n")
        checksums = consumer / "SHA256SUMS"
        checksums.write_text(f"{record['sha256']}  {record['archive']}\n", encoding="utf-8", newline="\n")
        bin_dir = consumer / "bin with spaces"
        install = installer_command(target, consumer, archive, checksums, bin_dir)
        run(install, cwd=consumer)
        checks.append("installer")
        executable = bin_dir / target["executable"]
        require(file_digest(executable) == record["executableSha256"], "The installer changed the executable bytes.")
        receipt_path = bin_dir / RECEIPT
        receipt_bytes = receipt_path.read_bytes()
        receipt = json.loads(receipt_bytes)
        require(same_path(receipt.get("executable", ""), str(executable)), "The receipt names another executable path.")
        if target["os"] != "win32":
            require(receipt["executable"] == os.path.join(os.path.realpath(bin_dir), target["executable"]),
                    "The receipt must name the physical executable path.")
        require(receipt == expected_receipt(version, target, record, receipt["executable"]),
                f"The installer wrote an unexpected receipt: {receipt}")
        checks.append("receipt")

        # No runtime, package manager, or development tool is on PATH.
        empty = consumer / "empty-path"
        empty.mkdir()
        env = dict(os.environ, PATH=str(empty), DSD_NO_UPDATE_CHECK="1")
        exe = str(executable)
        require(run([exe, "--version"], cwd=consumer, env=env) == f"dsd {version}\n", "--version mismatch.")
        run([exe, "--help"], cwd=consumer, env=env)
        reported = json.loads(run([exe, "version"], cwd=consumer, env=env))
        require(reported["ok"] and reported["result"]["version"] == version and reported["result"]["target"] == target["id"],
                f"dsd version reported {reported['result']}.")
        sample = ROOT / "fixtures" / "public" / "sample.dsd"
        checked = json.loads(run([exe, "check", str(sample), "--no-schema"], cwd=consumer, env=env))
        require(checked["ok"] and checked["result"]["passed"], "dsd check failed on the public fixture.")
        run([exe, "outline", str(sample), "--format", "text"], cwd=consumer, env=env)
        checks.append("smoke")

        # The installed binary accepts the installer's receipt. Contacting
        # GitHub may fail or find no release; only ownership errors fail here.
        proc = subprocess.run([exe, "upgrade", "--check"], cwd=consumer, env=dict(env, PATH=os.environ["PATH"]),
                              capture_output=True, **TEXT, timeout=120)
        upgrade = json.loads(proc.stdout)
        code = None if upgrade["ok"] else upgrade["error"]["code"]
        require(code in (None, "upgrade_release_invalid", "upgrade_source_unavailable"),
                f"dsd upgrade --check rejected the installation: {upgrade['error']}")
        checks.append("receipt-accepted")

        # A failed installation leaves the previous executable and receipt.
        checksums.write_text(f"{'0' * 64}  {record['archive']}\n", encoding="utf-8", newline="\n")
        rejected = subprocess.run(install, cwd=consumer, capture_output=True, **TEXT)
        require(rejected.returncode != 0, "A corrupt checksum must fail installation.")
        require(file_digest(executable) == record["executableSha256"], "A failed install changed the executable.")
        require(receipt_path.read_bytes() == receipt_bytes, "A failed install changed the receipt.")
        checks.append("checksum-recovery")
    result = dict(record, certified=True, os=target["os"], arch=target["arch"], checks=checks)
    report.parent.mkdir(parents=True, exist_ok=True)
    write_json(report, result)
    print(f"certified {target_id}: {', '.join(checks)}")
    return result


# ---------------------------------------------------------------------------
# bundle


def bundle_files(version: str) -> List[str]:
    return [archive_name(version, t) for t in TARGETS] + list(INSTALLERS) + [VERIFY_SCRIPT]


def checksum_text(directory: Path, names: Sequence[str]) -> str:
    return "".join(f"{file_digest(directory / name)}  {name}\n" for name in names)


def assemble(archives: Path, reports: Path, bundle: Path) -> dict:
    version = workspace_version()
    revision = source_revision()
    bundle.mkdir(parents=True, exist_ok=True)
    require(not any(bundle.iterdir()), f"{bundle} must start empty.")
    entries = []
    for target in TARGETS:
        record = read_build(archives, target, version, revision)
        report = json.loads((reports / f"{target['id']}.json").read_text(encoding="utf-8"))
        require(report.get("certified") is True and report.get("os") == target["os"] and report.get("arch") == target["arch"],
                f"{target['id']} lacks a native certification report.")
        for key, value in record.items():
            require(report.get(key) == value, f"{target['id']}: certification {key} differs from the build.")
        shutil.copyfile(archives / record["archive"], bundle / record["archive"])
        entries.append({"target": target["id"], "archive": record["archive"], "sha256": record["sha256"],
                        "executableSha256": record["executableSha256"], "runtimeCertified": True})
    compilers = {json.loads((archives / f"{t['id']}.json").read_text(encoding="utf-8"))["compiler"] for t in TARGETS}
    require(len(compilers) == 1, f"Targets were built with different compilers: {sorted(compilers)}.")
    for name, text in render_installers(version).items():
        (bundle / name).write_text(text, encoding="utf-8", newline="\n")
    shutil.copyfile(ROOT / "scripts" / VERIFY_SCRIPT, bundle / VERIFY_SCRIPT)
    files = bundle_files(version)
    (bundle / "SHA256SUMS").write_text(checksum_text(bundle, files), encoding="utf-8", newline="\n")
    manifest = {
        "version": version, "tag": f"v{version}", "sourceRevision": revision, "compiler": compilers.pop(),
        "targets": entries,
        "installers": [{"name": n, "sha256": file_digest(bundle / n)} for n in list(INSTALLERS) + [VERIFY_SCRIPT]],
    }
    write_json(bundle / "release-manifest.json", manifest)
    verify_bundle(bundle, revision)
    print(f"assembled {bundle} for v{version}")
    return manifest


def verify_bundle(bundle: Path, revision: Optional[str] = None) -> List[str]:
    """Checks a complete bundle against the inventory and this checkout; returns its asset names."""
    version = workspace_version()
    manifest = json.loads((bundle / "release-manifest.json").read_text(encoding="utf-8"))
    require(manifest["version"] == version and manifest["tag"] == f"v{version}", "Manifest version differs from Cargo.")
    require(manifest["sourceRevision"] == (revision or source_revision()), "Manifest source differs from the checkout.")
    require([e["target"] for e in manifest["targets"]] == [t["id"] for t in TARGETS], "Manifest target inventory differs.")
    for entry, target in zip(manifest["targets"], TARGETS):
        archive = (bundle / entry["archive"]).read_bytes()
        require(entry["archive"] == archive_name(version, target), f"Unexpected archive {entry['archive']}.")
        require(entry["sha256"] == digest(archive), f"{entry['archive']} checksum mismatch.")
        require(entry["executableSha256"] == digest(archive_executable(archive, target)), f"{entry['archive']} content mismatch.")
        require(entry["runtimeCertified"] is True, f"{target['id']} is not runtime-certified.")
    for name, text in render_installers(version).items():
        require((bundle / name).read_text(encoding="utf-8") == text, f"{name} differs from its source.")
    require((bundle / VERIFY_SCRIPT).read_bytes() == (ROOT / "scripts" / VERIFY_SCRIPT).read_bytes(),
            f"{VERIFY_SCRIPT} differs from its source.")
    require([i["name"] for i in manifest["installers"]] == list(INSTALLERS) + [VERIFY_SCRIPT], "Manifest installer list differs.")
    for item in manifest["installers"]:
        require(item["sha256"] == file_digest(bundle / item["name"]), f"{item['name']} checksum mismatch.")
    files = bundle_files(version)
    require((bundle / "SHA256SUMS").read_text(encoding="utf-8") == checksum_text(bundle, files), "SHA256SUMS differs.")
    assets = files + ["SHA256SUMS", "release-manifest.json"]
    require(sorted(p.name for p in bundle.iterdir()) == sorted(assets), "Bundle has missing or unexpected files.")
    return assets


# ---------------------------------------------------------------------------
# publication

Gh = Callable[[Sequence[str]], subprocess.CompletedProcess]
RELEASE_FIELDS = "tagName,isDraft,isPrerelease,name,body,url,assets"


def system_gh(args: Sequence[str]) -> subprocess.CompletedProcess:
    return subprocess.run(["gh", *args], capture_output=True, **TEXT, timeout=300)


def publish(repository: str, tag: str, sha: str, bundle: Path, gh: Gh = system_gh) -> str:
    """Creates a draft, uploads only missing assets after byte-comparing existing
    ones, rechecks the tag, publishes, and verifies the published release."""
    require(re.fullmatch(r"[A-Za-z0-9_.-]+/[A-Za-z0-9_.-]+", repository) is not None, "Invalid repository.")
    require(re.fullmatch(r"[0-9a-f]{40}", sha) is not None, "Invalid source SHA.")
    check_tag(tag, workspace_version())
    assets = verify_bundle(bundle, sha)

    def checked(args: Sequence[str]) -> str:
        proc = gh(args)
        require(proc.returncode == 0, f"gh {' '.join(args[:2])} failed: {(proc.stderr or proc.stdout).strip()}")
        return proc.stdout.strip()

    def verify_tag() -> None:
        # Resolve the tag itself, including annotated tags.
        resolved = checked(["api", f"repos/{repository}/commits/{tag}", "--jq", ".sha"])
        require(resolved == sha, "The remote tag no longer identifies the certified source.")

    def read_release(allow_draft: bool = True) -> Optional[dict]:
        proc = gh(["release", "view", tag, "--repo", repository, "--json", RELEASE_FIELDS])
        if proc.returncode != 0 and re.match(r"^release not found(?: \(HTTP 404\))?\s*$", proc.stderr, re.I):
            return None
        require(proc.returncode == 0, f"Could not inspect the GitHub Release: {(proc.stderr or proc.stdout).strip()}")
        release = json.loads(proc.stdout)
        require(release["tagName"] == tag, "The GitHub Release tag differs from the validated source.")
        require(release["isPrerelease"] is False, "The GitHub Release must not be a prerelease.")
        require(allow_draft or release["isDraft"] is False, "The GitHub Release must be published.")
        return release

    def verify_downloads(release: dict, complete: bool) -> None:
        names = sorted(asset["name"] for asset in release["assets"])
        require(len(set(names)) == len(names), "Duplicate release assets.")
        require(all(name in assets for name in names), f"Unexpected release assets: {names}.")
        if complete:
            require(names == sorted(assets), "The release is missing required assets.")
        if not names:
            return
        with tempfile.TemporaryDirectory(prefix="dsd-release-download-") as tmp:
            checked(["release", "download", tag, "--repo", repository, "--dir", tmp])
            require(sorted(os.listdir(tmp)) == names, "The downloaded release inventory changed.")
            for name in names:
                require(file_digest(Path(tmp) / name) == file_digest(bundle / name),
                        f"Remote {name} differs from the certified bundle; existing assets were not changed.")

    verify_tag()
    release = read_release()
    existed = release is not None
    if release is None:
        created = gh(["release", "create", tag, "--repo", repository, "--target", sha, "--verify-tag", "--draft",
                      "--title", tag, "--generate-notes"])
        # A concurrent run may have created it first; only a still-missing
        # release is a failure, reported with gh's own error.
        release = read_release()
        require(release is not None,
                f"The draft release could not be created: {(created.stderr or created.stdout).strip()}")
    verify_downloads(release, complete=not release["isDraft"])
    if release["isDraft"]:
        present = {asset["name"] for asset in release["assets"]}
        missing = [name for name in assets if name not in present]
        if missing:
            checked(["release", "upload", tag, "--repo", repository, *[str(bundle / n) for n in missing]])
        release = read_release()
        require(release is not None, "The draft disappeared during upload.")
        verify_downloads(release, complete=True)
        verify_tag()
        if release["isDraft"]:
            checked(["release", "edit", tag, "--repo", repository, "--draft=false"])
    release = read_release(allow_draft=False)
    require(release is not None, "The release disappeared after publication.")
    verify_downloads(release, complete=True)
    disposition = "verified-or-resumed" if existed else "published"
    print(f"{disposition}: {release['url']}")
    return disposition


# ---------------------------------------------------------------------------


def main(argv: Optional[Sequence[str]] = None) -> int:
    parser = argparse.ArgumentParser(description=__doc__.split("\n\n")[0])
    sub = parser.add_subparsers(dest="command", required=True)
    sub.add_parser("matrix", help="GitHub Actions matrix of the release targets")
    p = sub.add_parser("version", help="print the Cargo version; with --tag, require agreement")
    p.add_argument("--tag")
    p = sub.add_parser("build", help="build and pack one target")
    p.add_argument("target")
    p.add_argument("out", type=Path)
    p.add_argument("--executable", type=Path)
    p = sub.add_parser("verify-archive", help="check an archive's entries and binary format")
    p.add_argument("target")
    p.add_argument("archive", type=Path)
    p = sub.add_parser("certify", help="install and check an archive on its own platform")
    p.add_argument("target")
    p.add_argument("archives", type=Path)
    p.add_argument("report", type=Path)
    p = sub.add_parser("assemble", help="write the complete release bundle")
    p.add_argument("archives", type=Path)
    p.add_argument("reports", type=Path)
    p.add_argument("bundle", type=Path)
    p = sub.add_parser("verify-bundle", help="check a release bundle against this checkout")
    p.add_argument("bundle", type=Path)
    p.add_argument("--sha")
    p = sub.add_parser("publish", help="create or resume the GitHub Release")
    for flag in ("--repository", "--tag", "--sha", "--directory"):
        p.add_argument(flag, required=True)
    args = parser.parse_args(argv)
    try:
        if args.command == "matrix":
            print(json.dumps({"include": [{k: t[k] for k in ("id", "os", "runner", "rustTarget")} for t in TARGETS]}))
        elif args.command == "version":
            version = workspace_version()
            if args.tag is not None:
                check_tag(args.tag, version)
            print(version)
        elif args.command == "build":
            build(args.target, args.out.resolve(), args.executable and args.executable.resolve())
        elif args.command == "verify-archive":
            target = target_by_id(args.target)
            verify_binary(archive_executable(args.archive.read_bytes(), target), target)
            print(f"ok {args.archive.name}")
        elif args.command == "certify":
            certify(args.target, args.archives.resolve(), args.report.resolve())
        elif args.command == "assemble":
            assemble(args.archives.resolve(), args.reports.resolve(), args.bundle.resolve())
        elif args.command == "verify-bundle":
            verify_bundle(args.bundle.resolve(), args.sha)
            print(f"ok {args.bundle}")
        elif args.command == "publish":
            publish(args.repository, args.tag, args.sha, Path(args.directory).resolve())
    except ReleaseError as error:
        print(f"error: {error}", file=sys.stderr)
        if args.command == "publish":
            print("Release completion stopped. Existing tags and assets were not replaced; inspect the draft or "
                  "published release before retrying.", file=sys.stderr)
        return 1
    return 0


if __name__ == "__main__":
    sys.exit(main())
