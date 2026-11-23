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
import math
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


FOOTNOTES = re.compile(r"^(?P<figure>.*?)(?P<markers>(?:\s*\((?:\*(?:[0-9]+(?:,[0-9]+)*)?|주[0-9]+(?:,[0-9]+)*)\))+)$")


def figure(text: str):
    """The number and number format a table text should become, derived
    independently of the CLI; None when it must stay text. Footnote markers
    after a figure become a literal at the end of every format section."""
    t = text.strip()
    m = FOOTNOTES.match(t)
    if m and m["figure"].strip() and m["figure"].strip() != "-":
        plain = figure(m["figure"].strip())
        if plain is None:
            return None
        value, fmt = plain
        return value, ";".join(f'{section}"{m["markers"]}"' for section in fmt.split(";"))
    if m and m["figure"].strip() == "-":
        return None
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


CELL_TAGS = {"TH", "TD", "TE", "TU"}


def compact(text: str) -> str:
    """Text without any whitespace, Unicode included."""
    return "".join(text.split())


def container_cells(path: Path) -> dict:
    """{(table, row, k): text} for cells holding a nested table, where k is the
    0-based position of the cell in its TR and the text is the cell's own
    content with each nested table as the placeholder, read independently."""
    with zipfile.ZipFile(path) as zf:
        root = ET.fromstring(zf.read("contents.xml"))

    def own(el: ET.Element) -> str:
        if el.tag in SCAFFOLD:
            return ""
        if el.tag == "TABLE":
            return NESTED_MARK
        return (el.text or "") + "".join(own(c) + (c.tail or "") for c in el)

    found = {}
    for n, table in enumerate(root.iter("TABLE"), 1):
        rows = []
        for child in table:
            if child.tag == "TR":
                rows.append(child)
            elif child.tag in ("THEAD", "TBODY", "TFOOT"):
                rows.extend(tr for tr in child if tr.tag == "TR")
        for r, tr in enumerate(rows, 1):
            for k, cell in enumerate(c for c in tr if c.tag in CELL_TAGS):
                if cell.find(".//TABLE") is not None:
                    found[(n, r, k)] = compact(own(cell).replace("&cr;", ""))
    return found


def table_cell_ok(text: str, got, container) -> tuple[bool, bool]:
    """Whether a workbook cell matches its `dsd table` text, and whether it
    is a container cell. `container` is the cell's own text from the source
    when it holds a nested table: `dsd table` flattens the nested table into
    the cell, while the workbook shows a placeholder and places it apart."""
    if container is not None:
        return got is not None and got[0] == "s" and compact(got[1]) == container, True
    expect = figure(text)
    if text == "":
        return got is None or got == ("s", ""), False
    if expect is not None:
        return got == ("n", *expect), False
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
    nested = container_cells(src)
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
            k = 0  # position among the row's own cells, as in its TR
            for cell in row["cells"]:
                got = cells.get((top + row["row"] - 1, left + cell["col"] - 1))
                if "text" not in cell:
                    ok, is_container = got is None or got == ("s", ""), False  # covered or missing: empty
                else:
                    ok, is_container = table_cell_ok(cell["text"], got, nested.get((n, row["row"], k)))
                    k += 1
                mismatches += not ok
                containers += is_container
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
    foot_smoke(label, src, sheets, names, tmp)


CURRENCY = {"원": 1.0, "천원": 1e3, "백만원": 1e6, "억원": 1e8, "십억원": 1e9}
# Units that are not money but compare with themselves (`foot::PLAIN_UNITS`).
PLAIN_UNITS = {"주", "천주", "명", "개", "건", "회", "EA", "USD", "천USD", "EUR", "JPY", "CNY"}
UNIT_CAPTION = re.compile(r"단위[:：]([^)）,，;\]]+)")


def caption_unit(texts) -> str | None:
    for text in texts:
        m = UNIT_CAPTION.search(re.sub(r"\s", "", text))
        if m:
            return m.group(1)
    return None


def workbook_units(sheets: dict, names: dict) -> dict:
    """Each table's unit as the workbook shows it, read independently of the
    CLI: a caption in the table itself, else in the nearest row above it (a
    caption line, or the caption of the table that row belongs to)."""
    tables = {}
    for name, (sheet, first, last) in names.items():
        if name.startswith("DSD_T"):
            tables[int(name[5:])] = (sheet, first, last)

    def own(t):
        sheet, (top, left), (bottom, right) = tables[t]
        cells = sheets[sheet]
        return caption_unit(
            v[1] for (r, c), v in sorted(cells.items()) if top <= r <= bottom and left <= c <= right and v[0] == "s"
        )

    units = {}
    for t, (sheet, (top, _left), _last) in tables.items():
        unit = own(t)
        if unit is None:
            cells = sheets[sheet]
            above = [r for (r, _c), v in cells.items() if 3 <= r < top and v != ("s", "")]
            if above:
                row = max(above)
                holder = next((u for u, (s, f, l) in tables.items() if s == sheet and f[0] <= row <= l[0]), None)
                if holder is not None:
                    unit = own(holder)
                else:
                    unit = caption_unit(v[1] for (r, _c), v in cells.items() if r == row and v[0] == "s")
        units[t] = unit
    return units


def format_places(fmt: str) -> int:
    """Decimal places a number format shows (`foot::format_places`): a
    percentage holds two more as a fraction."""
    section = fmt.split(";")[0]
    section = re.sub(r'"[^"]*"', "", section)
    head, point, tail = section.partition(".")
    digits = sum(1 for c in tail if c in "0#") if point else 0
    return digits + (2 if "%" in section else 0)


def foot_smoke(label: str, src: Path, sheets: dict, names: dict, tmp: Path) -> None:
    """Re-evaluate every proposed check from the exported workbook's cells,
    independently of the CLI's arithmetic: same-table terms summed from the
    typed cells, statuses and differences compared. Then re-state proposals
    as agent checks in a checks file: each must apply in place with its
    status unchanged. Prints counts only."""
    envelope = dsd("foot", str(src))
    if not envelope["ok"]:
        fail(f"{label}: `foot` failed with {envelope['error']['code']}")
        return
    result = envelope["result"]

    def value(table: int, row: int, col: int):
        sheet, (top, left), _ = names[f"DSD_T{table}"]
        got = sheets[sheet].get((top + row - 1, left + col - 1))
        if got is None or got == ("s", ""):
            return None
        return got[1] if got[0] == "n" else "text"

    def places(table: int, row: int, col: int) -> int:
        sheet, (top, left), _ = names[f"DSD_T{table}"]
        got = sheets[sheet].get((top + row - 1, left + col - 1))
        return format_places(got[2]) if got and got[0] == "n" else 0

    units = workbook_units(sheets, names)

    def ratio(source: int, target: int):
        """The factor converting table `source` into `target`'s unit, from
        the captions the workbook shows; None when they cannot be compared."""
        if source == target:
            return 1.0
        a, b = units.get(source), units.get(target)
        if a and b and a in CURRENCY and b in CURRENCY:
            return CURRENCY[a] / CURRENCY[b]
        if a and a == b and a in PLAIN_UNITS:
            return 1.0
        return None

    disagreements = 0
    checks = result["checks"] + result["unresolved"] + result["candidates"]
    for check in checks:
        target = check["target"]
        factors = [ratio(term["table"], target["table"]) for term in check["terms"]]
        comparable = all(f is not None for f in factors)
        actual, expected, text = value(target["table"], target["row"], target["col"]), 0.0, False
        digits = places(target["table"], target["row"], target["col"])
        for term, factor in zip(check["terms"], factors):
            factor = factor or 1.0
            rows, cols = term.get("rows", [term.get("row")] * 2), term.get("cols", [term.get("col")] * 2)
            for row in range(rows[0], rows[1] + 1):
                for col in range(cols[0], cols[1] + 1):
                    got = value(term["table"], row, col)
                    text |= got == "text"
                    if isinstance(got, float):
                        expected += term["sign"] * got * factor
                        extra = round(-math.log10(factor)) if factor < 1 else 0
                        digits = max(digits, places(term["table"], row, col) + extra)
        if check["status"] == "needsJudgment":
            ok = text or not isinstance(actual, float) or not comparable
        else:
            # Units come from the captions the workbook shows, not from the
            # CLI's expected value; the difference is exact to the places the
            # figures show.
            digits = min(digits, 9)
            difference = round(actual - expected, digits) + 0.0 if isinstance(actual, float) else None
            allowed = check["allowed"]
            holds = difference is not None and abs(difference) <= allowed + 1e-12 * max(1.0, allowed)
            near = lambda a, b: abs(a - b) <= 1e-12 * max(1.0, abs(a), abs(b))  # noqa: E731
            ok = (
                comparable
                and not text
                and difference is not None
                and near(check["actual"], actual)
                and near(check["expected"], expected)
                and abs(check["difference"] - difference) <= 10.0 ** -(digits + 1)
                and (check["status"] == "pass") == holds
            )
        disagreements += not ok
    counts = result["counts"]
    tally = Counter(c["status"] for c in result["checks"])
    consistent = (
        (counts["pass"], counts["fail"], counts["needsJudgment"]) == (tally["pass"], tally["fail"], tally["needsJudgment"])
        and counts["unresolved"] == len(result["unresolved"])
        and counts["candidates"] == len(result["candidates"])
        and counts["uncovered"] == len(result["uncovered"])
        and len({c["id"] for c in checks}) == len(checks)
    )
    if disagreements:
        fail(f"{label}: foot disagrees with the workbook on {disagreements} check(s)")
    if not consistent:
        fail(f"{label}: foot counts or ids are inconsistent")
    overlaid = overlay_smoke(label, src, result["checks"], tmp)
    formulas = footing_workbook_smoke(label, src, result["checks"], tmp)
    if not disagreements and consistent and overlaid is not None and formulas is not None:
        print(
            f"ok   {label}: foot {counts['pass']} pass, {counts['fail']} fail, {counts['needsJudgment']} needing judgment, "
            f"{counts['unresolved']} unresolved, {counts['candidates']} candidates, {counts['uncovered']} uncovered; "
            f"re-evaluated from the workbook; {overlaid} re-stated in a checks file; "
            f"{formulas} workbook formulas evaluated"
        )


def read_formulas(path: Path) -> dict:
    """Formulas per sheet as {(row, col): formula without "="}."""
    with zipfile.ZipFile(path) as zf:
        book = ET.fromstring(zf.read("xl/workbook.xml"))
        rels = {r.get("Id"): r.get("Target") for r in ET.fromstring(zf.read("xl/_rels/workbook.xml.rels"))}
        out = {}
        for sheet in book.iter(f"{XL}sheet"):
            target = rels[sheet.get(f"{XL_REL}id")].lstrip("/")
            target = target if target.startswith("xl/") else "xl/" + target
            cells = {}
            for c in ET.fromstring(zf.read(target)).iter(f"{XL}c"):
                f = c.find(f"{XL}f")
                if f is not None and f.text:
                    cells[cell_position(c.get("r"))] = f.text
            out[sheet.get("name")] = cells
    return out


REFERENCE = re.compile(r"(?:'((?:[^']|'')+)'!)?(\$?[A-Z]+\$?\d+)(?::(\$?[A-Z]+\$?\d+))?")
DIFFERENCE = re.compile(r"^ROUND\((?P<target>(?:'(?:[^']|'')+'!)?\$?[A-Z]+\$?\d+)-\((?P<terms>.*)\),(?P<places>-?\d+)\)$")
TERM = re.compile(r"(?P<sign>[+-]?)(?:(?P<factor>[0-9.e+-]+)\*)?SUM\((?P<refs>[^)]+)\)")


def evaluate_difference(formula: str, home: str, sheets: dict, formulas: dict):
    """Evaluate `ROUND(target-(±[k*]SUM(refs)...),places)` or a reference to such a
    cell, independently of the CLI, from the workbook's cell values. Returns
    None for a formula of another shape."""

    def resolve(ref: str):
        m = REFERENCE.fullmatch(ref)
        sheet = m.group(1).replace("''", "'") if m.group(1) else home
        return sheet, cell_position(m.group(2)), cell_position(m.group(3)) if m.group(3) else None

    def number(sheet, pos):
        got = sheets.get(sheet, {}).get(pos)
        return got[1] if got and got[0] == "n" else 0.0

    if REFERENCE.fullmatch(formula):
        sheet, pos, _ = resolve(formula)
        inner = formulas.get(sheet, {}).get(pos)
        return evaluate_difference(inner, sheet, sheets, formulas) if inner else None
    m = DIFFERENCE.match(formula)
    if m is None:
        return None
    sheet, pos, _ = resolve(m["target"])
    total, rest = 0.0, m["terms"]
    for term in TERM.finditer(rest):
        part = 0.0
        for ref in REFERENCE.finditer(term["refs"]):
            s, first, last = resolve(ref.group(0))
            last = last or first
            part += sum(number(s, (r, c)) for r in range(first[0], last[0] + 1) for c in range(first[1], last[1] + 1))
        factor = float(term["factor"]) if term["factor"] else 1.0
        total += (-1 if term["sign"] == "-" else 1) * factor * part
    return round(number(sheet, pos) - total, int(m["places"])) + 0.0


def footing_workbook_smoke(label: str, src: Path, checks: list, tmp: Path):
    """Write the footing workbook twice (byte-identical), then evaluate every
    check's formula on the 검증 sheet from the workbook's own cells and
    require the CLI's difference. Returns how many formulas were evaluated,
    or None after a failure."""
    out, again = tmp / "foot-a.xlsx", tmp / "foot-b.xlsx"
    envelope = dsd("foot", str(src), "-o", str(out))
    if not envelope["ok"] or not dsd("foot", str(src), "-o", str(again))["ok"]:
        fail(f"{label}: `foot -o` failed")
        return None
    if out.read_bytes() != again.read_bytes():
        fail(f"{label}: the footing workbook is not byte-identical across runs")
        return None
    sheets, _ = read_workbook(out)
    formulas = read_formulas(out)
    by_id = {c["id"]: c for c in checks}
    name = list(sheets)[-1]  # the checks sheet is last
    listing = sheets[name]
    rows = sorted({r for (r, _c) in listing if r >= 3})
    evaluated = wrong = 0
    for r in rows:
        check = by_id.get(listing.get((r, 2), ("s", ""))[1])
        if check is None:
            wrong += 1
            continue
        formula = formulas[name].get((r, 7))
        if check["status"] == "needsJudgment" or formula is None:
            wrong += formula is not None or listing.get((r, 9), ("s", ""))[1] != "판단 필요"
            continue
        got = evaluate_difference(formula, name, sheets, formulas)
        if got is None:
            wrong += 1
            continue
        # Both are rounded to the places the figures carry: they agree exactly.
        wrong += abs(got - check["difference"]) > 1e-9 * max(1.0, abs(got))
        evaluated += 1
    if len(rows) != len(checks):
        fail(f"{label}: the 검증 sheet lists {len(rows)} of {len(checks)} checks")
        return None
    if wrong:
        fail(f"{label}: {wrong} footing workbook formula(s) disagree with `foot`")
        return None
    return evaluated


def overlay_smoke(label: str, src: Path, checks: list, tmp: Path):
    """Dismiss the first proposal and re-state the next ones as agent checks:
    every entry's labels are where it says, so all apply and nothing moves.
    Returns how many entries applied, or None after a failure."""
    sample = checks[:41]
    if len(sample) < 2:
        return 0
    overlay = {
        "checks": [
            {"id": c["id"], "kind": c["kind"], "origin": "agent", "target": c["target"], "terms": c["terms"], "note": "smoke"}
            for c in sample[1:]
        ],
        "dismiss": [{"id": sample[0]["id"], "target": sample[0]["target"], "reason": "smoke"}],
    }
    path = tmp / "checks.json"
    path.write_text(json.dumps(overlay, ensure_ascii=False), encoding="utf-8")
    envelope = dsd("foot", str(src), "--checks", str(path))
    if not envelope["ok"]:
        fail(f"{label}: `foot --checks` failed with {envelope['error']['code']}")
        return None
    result = envelope["result"]
    statuses = {c["id"]: (c["origin"], c["status"]) for c in result["checks"]}
    applied = all(statuses.get(c["id"]) == ("agent", c["status"]) for c in sample[1:])
    c = result["counts"]
    if not applied or (c["dismissed"], c["moved"], c["stale"]) != (1, 0, 0) or sample[0]["id"] in statuses:
        fail(f"{label}: a checks file re-stating proposals did not apply in place")
        return None
    return len(sample) - 1


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
