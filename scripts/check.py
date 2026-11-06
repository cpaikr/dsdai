#!/usr/bin/env python3
"""Repository checks: skill package rules, Rust and release-tooling tests, and a
CLI smoke run over the public fixture and the private corpus.

Usage: python scripts/check.py

Private samples are included only when fixtures/private/ exists; output names
sample indexes and counts, never document text or file names.
"""

from __future__ import annotations

import json
from collections import Counter
import os
import re
import subprocess
import sys
import tempfile
import unittest
import xml.etree.ElementTree as ET
import zipfile
from pathlib import Path

ROOT = Path(__file__).resolve().parent.parent
SKILLS = ROOT / "skills"
PRIVATE = ROOT / "fixtures" / "private" / "audit-dsd"
ALLOWED_FIELDS = {"name", "description", "license", "compatibility", "metadata", "allowed-tools"}
EXEMPT_DIRS = {"agents", "evals"}

errors: list = []


def fail(msg: str) -> None:
    errors.append(msg)
    print(f"FAIL {msg}")


def check_skill(skill: Path) -> None:
    entry = skill / "SKILL.md"
    if not entry.is_file():
        fail(f"{skill.name}: missing SKILL.md")
        return
    text = entry.read_text(encoding="utf-8")
    m = re.match(r"---\n(.*?)\n---\n", text, re.S)
    if not m:
        fail(f"{skill.name}: SKILL.md lacks YAML frontmatter")
        return
    fields = {}
    for line in m.group(1).splitlines():
        key, _, value = line.partition(":")
        fields[key.strip()] = value.strip()
    if set(fields) - ALLOWED_FIELDS:
        fail(f"{skill.name}: unsupported frontmatter fields {sorted(set(fields) - ALLOWED_FIELDS)}")
    if fields.get("name") != skill.name:
        fail(f"{skill.name}: frontmatter name {fields.get('name')!r} must match directory")
    if not fields.get("description"):
        fail(f"{skill.name}: missing description")
    body = text[m.end():]
    links = re.findall(r"\]\(<?([^)>]+)>?\)", body)
    for link in links:
        if re.match(r"[a-z]+://", link) or link.startswith("#"):
            continue
        target = (skill / link).resolve()
        if not str(target).startswith(str(skill.resolve())):
            fail(f"{skill.name}: link escapes the skill directory: {link}")
        elif not target.exists():
            fail(f"{skill.name}: broken link {link}")
    for path in skill.rglob("*"):
        rel = path.relative_to(skill)
        if path.is_symlink():
            fail(f"{skill.name}: symlink {rel}")
        if path.is_file() and rel.parts[0] not in EXEMPT_DIRS and rel.name != "SKILL.md" and "__pycache__" not in rel.parts:
            if rel.as_posix() not in body:
                fail(f"{skill.name}: runtime file {rel.as_posix()} is not named in SKILL.md")
    print(f"ok   package {skill.name}")


def run_cargo() -> None:
    """Format, lint, and test the Rust workspace (this also checks that the
    public fixture matches fixtures/public/sample/). Tests run with the
    test-only fixture-origin feature, which enables the release-check and
    upgrade tests against a local release origin; lints cover both builds."""
    for args in (
        ["fmt", "--all", "--check"],
        ["clippy", "--workspace", "--all-targets", "--locked", "--", "-D", "warnings"],
        ["clippy", "--workspace", "--all-targets", "--all-features", "--locked", "--", "-D", "warnings"],
        ["test", "--workspace", "--all-features", "--locked"],
    ):
        proc = subprocess.run(["cargo", *args], cwd=ROOT)
        label = args[0] + (" --all-features" if "--all-features" in args else "")
        if proc.returncode != 0:
            fail(f"cargo {label}")
        else:
            print(f"ok   cargo {label}")


def run_release_tests() -> None:
    """Unit tests for the release tooling in scripts/release.py."""
    tests = ROOT / "scripts" / "tests"
    suite = unittest.defaultTestLoader.discover(str(tests), pattern="test_*.py", top_level_dir=str(tests))
    result = unittest.TextTestRunner(verbosity=0, stream=sys.stdout).run(suite)
    if not result.wasSuccessful():
        fail("release tooling unit tests")
    else:
        print(f"ok   {result.testsRun} release tooling unit tests")


SCAFFOLD = {"COMMENT", "LIBRARYLIST", "FILENAME"}
def executable() -> Path:
    """The debug `dsd` in Cargo's target directory, honoring CARGO_TARGET_DIR."""
    proc = subprocess.run(["cargo", "metadata", "--format-version", "1", "--no-deps"], cwd=ROOT, capture_output=True)
    target = Path(json.loads(proc.stdout)["target_directory"]) if proc.returncode == 0 else ROOT / "target"
    return target / "debug" / ("dsd.exe" if sys.platform == "win32" else "dsd")


EXE = executable()


def dsd(*args: str) -> dict:
    proc = subprocess.run([str(EXE), *args], capture_output=True, env={**os.environ, "DSD_NO_UPDATE_CHECK": "1"})
    return json.loads(proc.stdout.decode("utf-8"))


def squeeze(text: str) -> str:
    return re.sub(r"[ \t\r\n]|⟦[^⟧]*⟧", "", text)


def source_text(path: Path) -> str:
    """Text outside tables and editor scaffolding, read independently with ElementTree."""
    with zipfile.ZipFile(path) as zf:
        root = ET.fromstring(zf.read("contents.xml"))

    def walk(el: ET.Element) -> str:
        if el.tag in SCAFFOLD or el.tag == "TABLE":
            return ""
        return (el.text or "") + "".join(walk(c) + (c.tail or "") for c in el)

    return squeeze(walk(root).replace("&cr;", ""))


def corpus_smoke() -> None:
    """Run the built CLI over the public fixture and any private samples.

    Prints sample indexes and counts only, never document text or file names.
    """
    files = [ROOT / "fixtures" / "public" / "sample.dsd"]
    files += sorted(PRIVATE.glob("*.dsd")) if PRIVATE.is_dir() else []
    if not EXE.is_file():
        fail(f"{EXE} is missing; cargo test builds it")
        return
    with tempfile.TemporaryDirectory() as tmp:
        for i, src in enumerate(files):
            label = "public fixture" if i == 0 else f"private sample #{i}"
            work, out = Path(tmp) / f"w{i}", Path(tmp) / f"o{i}.dsd"
            steps = [
                ("check", ["check", str(src)]),
                ("read", ["read", str(src)]),
                ("outline", ["outline", str(src)]),
                ("extract", ["extract", str(src), str(work)]),
                ("pack", ["pack", str(work), str(src), "-o", str(out)]),
            ]
            results = {}
            for name, args in steps:
                envelope = dsd(*args)
                if not envelope["ok"]:
                    fail(f"{label}: `{name}` failed with {envelope['error']['code']}")
                    break
                results[name] = envelope["result"]
            else:
                # Unverified elements or schemas prompt verifying the CLI on this
                # sample and extending OBSERVED_TAGS; they are not failures.
                # Codes and counts only: messages can carry document values.
                unverified = Counter(i["code"] for i in results["check"]["issues"] if i["code"].startswith("unverified-"))
                for code, count in sorted(unverified.items()):
                    print(f"note {label}: {code} x{count}")
                shown = squeeze("".join(r.get("text", "") for r in results["read"]["rows"]))
                if shown != source_text(src):
                    fail(f"{label}: read omits or alters text outside tables")
                elif out.read_bytes() != src.read_bytes():
                    fail(f"{label}: extract/pack round trip is not byte-identical")
                else:
                    print(f"ok   {label}: check, complete read, outline, byte-identical round trip")


def main() -> int:
    for skill in sorted(p for p in SKILLS.iterdir() if p.is_dir()):
        check_skill(skill)
    run_cargo()
    run_release_tests()
    corpus_smoke()
    if errors:
        print(f"\n{len(errors)} check(s) failed")
        return 1
    print("\nall checks passed")
    return 0


if __name__ == "__main__":
    sys.exit(main())
