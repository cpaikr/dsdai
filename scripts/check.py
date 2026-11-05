#!/usr/bin/env python3
"""Repository checks: skill package rules, unit tests, and private-corpus smoke.

Usage: python scripts/check.py

The private-corpus step runs only when fixtures/private/ exists and prints
file indexes and counts, never document text or file names.
"""

from __future__ import annotations

import json
import re
import subprocess
import sys
import tempfile
import unittest
from pathlib import Path

ROOT = Path(__file__).resolve().parent.parent
SKILLS = ROOT / "skills"
PRIVATE = ROOT / "fixtures" / "private" / "audit-dsd"
PUBLIC_FIXTURE = ROOT / "fixtures" / "public" / "sample.dsd"
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


def run_unit_tests() -> None:
    for evals in sorted(SKILLS.glob("*/evals")):
        suite = unittest.defaultTestLoader.discover(str(evals), pattern="test_*.py", top_level_dir=str(evals))
        result = unittest.TextTestRunner(verbosity=0, stream=sys.stdout).run(suite)
        if not result.wasSuccessful():
            fail(f"unit tests in {evals.relative_to(ROOT)}")
        else:
            print(f"ok   {result.testsRun} unit tests in {evals.relative_to(ROOT).as_posix()}")


def check_public_fixture() -> None:
    sys.path.insert(0, str(SKILLS / "dart-dsd" / "evals"))
    import make_fixture  # noqa: E402

    if not PUBLIC_FIXTURE.is_file() or PUBLIC_FIXTURE.read_bytes() != make_fixture.fixture_bytes():
        fail(f"{PUBLIC_FIXTURE.relative_to(ROOT)} is stale; regenerate with skills/dart-dsd/evals/make_fixture.py")
    else:
        print("ok   public fixture matches its generator")


def private_smoke() -> None:
    files = sorted(PRIVATE.glob("*.dsd")) if PRIVATE.is_dir() else []
    if not files:
        print("skip private corpus (fixtures/private/ absent)")
        return
    tool = SKILLS / "dart-dsd" / "scripts" / "dsd.py"
    with tempfile.TemporaryDirectory() as tmp:
        for i, src in enumerate(files, 1):
            work, out, md = Path(tmp) / f"w{i}", Path(tmp) / f"o{i}.dsd", Path(tmp) / f"m{i}.md"
            steps = [
                ["check", str(src), "--json"],
                ["read", str(src), "-o", str(md)],
                ["outline", str(src)],
                ["extract", str(src), str(work)],
                ["pack", str(work), str(src), "-o", str(out)],
            ]
            for step in steps:
                proc = subprocess.run([sys.executable, "-I", str(tool), *step], capture_output=True)
                if proc.returncode != 0:
                    fail(f"private sample #{i}: `{step[0]}` exited {proc.returncode}")
                    break
                if step[0] == "check":
                    # Unverified elements or schemas are a prompt to verify the
                    # helper on this sample and extend OBSERVED_TAGS, not a failure.
                    for issue in json.loads(proc.stdout):
                        if issue["code"].startswith("unverified-"):
                            print(f"note private sample #{i}: {issue['code']} {issue['message'].split(':')[0]}")
            else:
                if out.read_bytes() != src.read_bytes():
                    fail(f"private sample #{i}: extract/pack round trip is not byte-identical")
                else:
                    print(f"ok   private sample #{i}: check, read, outline, byte-identical round trip")


def main() -> int:
    for skill in sorted(p for p in SKILLS.iterdir() if p.is_dir()):
        check_skill(skill)
    check_public_fixture()
    run_unit_tests()
    private_smoke()
    if errors:
        print(f"\n{len(errors)} check(s) failed")
        return 1
    print("\nall checks passed")
    return 0


if __name__ == "__main__":
    sys.exit(main())
