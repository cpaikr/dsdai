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
from decimal import Decimal
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


# ---------------------------------------------------------------------------
# xlsx export: the workbook is read back with the standard library only.

XL = "{http://schemas.openxmlformats.org/spreadsheetml/2006/main}"
XL_REL = "{http://schemas.openxmlformats.org/officeDocument/2006/relationships}"
BLOCK_TAGS = {"P", "TITLE", "COVER-TITLE", "IMG-CAPTION", "WARNING", "SUBTITLE", "IMG"}
INDEX_SHEET = "총괄표"
IMAGE_MARK = "[이미지] "
NESTED_MARK = "[표]"
UNMAPPED_LINE = re.compile(r"^\[T\d+\] \d+x\d+ table ")
FIGURE = re.compile(
    r"^(?P<open>[(△▲-])?\s*(?P<int>[0-9]{1,3}(?:,[0-9]{3})+|[0-9]+)(?:\.(?P<frac>[0-9]+))?(?P<pct>%)?\s*(?P<close>\))?$"
)
# Excel's built-in number formats the writer refers to by id instead of by code.
BUILTIN_FORMATS = {0: "General", 1: "0", 2: "0.00", 3: "#,##0", 4: "#,##0.00", 9: "0%", 10: "0.00%"}


def cell_position(ref: str) -> tuple[int, int]:
    """0-based (row, col) of an A1 reference, with or without `$`."""
    m = re.match(r"\$?([A-Z]+)\$?(\d+)$", ref)
    col = 0
    for ch in m.group(1):
        col = col * 26 + ord(ch) - 64
    return int(m.group(2)) - 1, col - 1


def read_workbook(path: Path) -> tuple[dict, dict]:
    """Cells per sheet as {(row, col): ("s", text) | ("n", float, number format)},
    and each defined name as (sheet, first cell, last cell)."""
    with zipfile.ZipFile(path) as zf:
        book = ET.fromstring(zf.read("xl/workbook.xml"))
        styles = ET.fromstring(zf.read("xl/styles.xml"))
        codes = dict(BUILTIN_FORMATS)
        codes.update({int(f.get("numFmtId")): f.get("formatCode") for f in styles.iter(f"{XL}numFmt")})
        xfs = styles.find(f"{XL}cellXfs")
        cell_formats = [codes.get(int(xf.get("numFmtId", "0")), "?") for xf in xfs.iter(f"{XL}xf")]
        rels = {r.get("Id"): r.get("Target") for r in ET.fromstring(zf.read("xl/_rels/workbook.xml.rels"))}
        shared = []
        if "xl/sharedStrings.xml" in zf.namelist():
            for si in ET.fromstring(zf.read("xl/sharedStrings.xml")).iter(f"{XL}si"):
                shared.append("".join(t.text or "" for t in si.iter(f"{XL}t")))
        sheets = {}
        for sheet in book.iter(f"{XL}sheet"):
            target = rels[sheet.get(f"{XL_REL}id")].lstrip("/")
            target = target if target.startswith("xl/") else "xl/" + target
            cells = {}
            for c in ET.fromstring(zf.read(target)).iter(f"{XL}c"):
                v, kind, pos = c.find(f"{XL}v"), c.get("t"), cell_position(c.get("r"))
                if kind == "s" and v is not None:
                    cells[pos] = ("s", shared[int(v.text)])
                elif kind == "inlineStr":
                    cells[pos] = ("s", "".join(t.text or "" for t in c.iter(f"{XL}t")))
                elif kind == "str" and v is not None:
                    cells[pos] = ("s", v.text or "")
                elif v is not None:
                    cells[pos] = ("n", float(v.text), cell_formats[int(c.get("s", "0"))])
            sheets[sheet.get("name")] = cells
        names = {}
        for dn in book.iter(f"{XL}definedName"):
            sheet, _, area = dn.text.lstrip("=").rpartition("!")
            first, _, last = area.partition(":")
            sheet = sheet[1:-1].replace("''", "'") if sheet.startswith("'") else sheet
            names[dn.get("name")] = (sheet, cell_position(first), cell_position(last or first))
    return sheets, names


def figure(text: str):
    """The number and number format a table text should become, derived
    independently of the CLI; None when it must stay text."""
    t = text.strip()
    if t == "-":
        return 0.0, '#,##0;-#,##0;"-"'
    m = FIGURE.match(t)
    if not m or (m["open"] == "(") != bool(m["close"]):
        return None
    digits = m["int"].replace(",", "")
    if m["int"].startswith("0") and ("," in m["int"] or len(digits) > 1):
        return None
    if len((digits + (m["frac"] or "")).lstrip("0")) > 15:
        return None
    value = Decimal(digits + ("." + m["frac"] if m["frac"] else ""))
    if m["pct"]:
        value /= 100
    fmt = ("#,##0" if "," in m["int"] else "0") + ("." + "0" * len(m["frac"]) if m["frac"] else "") + (m["pct"] or "")
    if m["open"]:
        if value == 0:
            return None
        value = -value
        fmt += {"(": f";({fmt})", "-": f";-{fmt}"}.get(m["open"], f';"{m["open"]}"{fmt}')
    return float(value), fmt


def source_lines(path: Path) -> Counter:
    """Squeezed text lines outside tables, scaffolding, and the document
    header, read independently with ElementTree."""
    with zipfile.ZipFile(path) as zf:
        root = ET.fromstring(zf.read("contents.xml"))
    lines: Counter = Counter()
    buf: list = []

    def flush() -> None:
        for line in "".join(buf).replace("\r", "").replace("\n", "").split("&cr;"):
            line = squeeze(line.rstrip())  # str.rstrip trims Unicode whitespace, as the exporter does
            if line and line != NESTED_MARK:
                lines[line] += 1
        buf.clear()

    def walk(el: ET.Element) -> None:
        if el.tag in SCAFFOLD or el.tag == "DOCUMENT-HEADER":
            return
        if el.tag == "TABLE":
            flush()
            return
        block = el.tag in BLOCK_TAGS
        if block:
            flush()
        buf.append(el.text or "")
        for child in el:
            walk(child)
            buf.append(child.tail or "")
        if block:
            flush()

    walk(root)
    flush()
    return lines


def table_cell_ok(text: str, got) -> tuple[bool, bool]:
    """Whether a workbook cell matches its `dsd table` text, and whether it
    is a container cell showing a nested table as a placeholder."""
    expect = figure(text)
    if text == "":
        return got is None or got == ("s", ""), False
    if expect is not None:
        return got == ("n", *expect), False
    if got and got[0] == "s" and got[1] != text and NESTED_MARK in got[1].split("\n"):
        # `dsd table` flattens the nested table into the cell, possibly within a
        # line; the workbook places it on its own. The container's own text must
        # appear in order in the flattened text.
        flat, at = squeeze(text), 0
        for part in (squeeze(line) for line in got[1].split("\n") if line != NESTED_MARK):
            at = flat.find(part, at)
            if at < 0:
                return False, True
            at += len(part)
        return True, True
    return got == ("s", text), False


def xlsx_smoke(label: str, src: Path, tmp: Path) -> None:
    """Export twice (byte-identical), read the workbook back, and require
    every table origin cell to match `dsd table` and every text line to be
    present. Prints counts only."""
    out, again = tmp / "a.xlsx", tmp / "b.xlsx"
    envelope = dsd("xlsx", str(src), "-o", str(out))
    if not envelope["ok"]:
        fail(f"{label}: `xlsx` failed with {envelope['error']['code']}")
        return
    result = envelope["result"]
    if not dsd("xlsx", str(src), "-o", str(again))["ok"] or out.read_bytes() != again.read_bytes():
        fail(f"{label}: xlsx output is not byte-identical across runs")
    sheets, names = read_workbook(out)
    unmapped = sum(1 for w in envelope["warnings"] if w["code"] == "table-not-mapped")
    count = dsd("read", str(src))["result"]["tables"]
    numbers = [t["table"] for t in result["tables"]]
    if len(numbers) + unmapped != count or numbers != sorted(set(numbers)):
        fail(f"{label}: xlsx maps {len(numbers)} of {count} tables ({unmapped} reported unmapped)")
    views = {t["index"]: t for t in dsd("table", str(src), *map(str, numbers))["result"]["tables"]} if numbers else {}
    mismatches = containers = 0
    claimed: dict = {}  # per sheet: table grid positions and markers, which are not text lines
    for entry in result["tables"]:
        n = entry["table"]
        sheet, (top, left), (bottom, right) = names[f"DSD_T{n}"]
        view = views[n]
        if (sheet, bottom - top + 1, right - left + 1) != (entry["sheet"], view["rowCount"], view["colCount"]):
            mismatches += 1
        cells, mine = sheets[sheet], claimed.setdefault(sheet, set())
        mine.add((top, left + view["colCount"]))
        mine.update((top + r, left + c) for r in range(view["rowCount"]) for c in range(view["colCount"]))
        for row in view["rows"]:
            for cell in row["cells"]:
                got = cells.get((top + row["row"] - 1, left + cell["col"] - 1))
                if "text" not in cell:
                    ok, nested = got is None or got == ("s", ""), False  # covered or missing: empty
                else:
                    ok, nested = table_cell_ok(cell["text"], got)
                mismatches += not ok
                containers += nested
    shown: Counter = Counter()
    for name, cells in sheets.items():
        if name == INDEX_SHEET:
            continue
        for pos, (kind, value, *_) in cells.items():
            # Rows 1-2 hold the sheet title and the index link.
            if pos[0] < 3 or pos in claimed.get(name, set()) or kind != "s" or UNMAPPED_LINE.match(value):
                continue
            value = value.removeprefix(IMAGE_MARK)
            for line in value.split("\n"):
                # A paragraph's nested table is placed on its own; its placeholder is not source text.
                if squeeze(line) and line != NESTED_MARK:
                    shown[squeeze(line)] += 1
    expected = source_lines(src)
    missing, extra = sum((expected - shown).values()), sum((shown - expected).values())
    if mismatches:
        fail(f"{label}: xlsx has {mismatches} table cell mismatch(es)")
    if missing or extra:
        fail(f"{label}: xlsx text lines differ ({missing} missing, {extra} extra)")
    if not mismatches and not missing and not extra:
        c = result["counts"]
        print(
            f"ok   {label}: xlsx {c['sheets']} sheets, {c['tables']} tables, {c['numberCells']} number and "
            f"{c['textCells']} text cells, {sum(expected.values())} text lines, {containers} nested-table container(s)"
        )


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
            xlsx_dir = Path(tmp) / f"x{i}"
            xlsx_dir.mkdir()
            xlsx_smoke(label, src, xlsx_dir)


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
