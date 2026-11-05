#!/usr/bin/env python3
"""Compare the Rust `dsd` CLI with the Python reference helper.

Temporary cutover evidence: runs both implementations over the public
fixture and, when present, every private sample, and compares text views,
check and inspect results, edit outputs, and diffs. Prints sample indexes and
pass/fail only, never document text or file names.

Usage: python scripts/parity.py [path/to/dsd]
"""

from __future__ import annotations

import json
import subprocess
import sys
import tempfile
import zipfile
from collections import Counter
from pathlib import Path

ROOT = Path(__file__).resolve().parent.parent
PY = ROOT / "skills" / "dart-dsd" / "scripts" / "dsd.py"
RUST = Path(sys.argv[1]) if len(sys.argv) > 1 else ROOT / "target" / "debug" / ("dsd.exe" if sys.platform == "win32" else "dsd")
SAMPLES = [ROOT / "fixtures" / "public" / "sample.dsd"] + sorted((ROOT / "fixtures" / "private" / "audit-dsd").glob("*.dsd"))

failures: list = []


def py(*args: str) -> subprocess.CompletedProcess:
    return subprocess.run([sys.executable, "-I", str(PY), *args], capture_output=True)


def rs(*args: str) -> subprocess.CompletedProcess:
    return subprocess.run([str(RUST), *args], capture_output=True)


def text(proc: subprocess.CompletedProcess) -> str:
    return proc.stdout.decode("utf-8").replace("\r\n", "\n")


def envelope(proc: subprocess.CompletedProcess) -> dict:
    return json.loads(proc.stdout.decode("utf-8"))


def same(label: str, a: object, b: object) -> bool:
    if a != b:
        failures.append(label)
        print(f"FAIL {label}")
        return False
    return True


def contents(path: Path) -> bytes:
    with zipfile.ZipFile(path) as zf:
        return zf.read("contents.xml")


def members(path: Path) -> list:
    with zipfile.ZipFile(path) as zf:
        return [(i.orig_filename, i.flag_bits, i.date_time, zf.read(i)) for i in zf.infolist()]


def views(i: int, src: Path) -> None:
    s = str(src)
    for args in (["outline", s], ["outline", s, "--all"], ["read", s, "--no-header"], ["read", s, "--no-header", "--scaffold"]):
        same(f"#{i} {' '.join(a for a in args if a != s)}", text(py(*args)), text(rs(*args, "--format", "text")))
    tables = envelope(rs("inspect", s))["result"]["document"]["counts"].get("TABLE", 0)
    if tables:
        nums = [str(n) for n in range(1, tables + 1)]
        same(f"#{i} table all", text(py("table", s, *nums)), text(rs("table", s, *nums, "--format", "text")))
        same(f"#{i} table all --spans", text(py("table", s, *nums, "--spans")), text(rs("table", s, *nums, "--spans", "--format", "text")))
    blocks = [r for r in envelope(rs("read", s))["result"]["rows"] if r.get("text", "").strip()]
    for q in {b["text"].strip()[:4] for b in blocks[:: max(1, len(blocks) // 6)]}:
        for extra in ([], ["--exact"]):
            same(f"#{i} find{' --exact' if extra else ''}", text(py("find", s, q, *extra, "--limit", "1000")),
                 text(rs("find", s, q, *extra, "--limit", "1000", "--format", "text")))


def checks(i: int, src: Path) -> None:
    s = str(src)
    for extra in (["--no-schema"], []):
        p = json.loads(py("check", s, "--json", *extra).stdout)
        r = envelope(rs("check", s, *extra))
        issues = r["result"]["issues"] if r["result"] else []
        same(f"#{i} check {' '.join(extra)} issue codes", Counter((x["level"], x["code"]) for x in p),
             Counter((x["level"], x["code"]) for x in issues))
    p = json.loads(py("inspect", s, "--json").stdout)
    r = envelope(rs("inspect", s))["result"]
    same(f"#{i} inspect counts", p.get("document", {}).get("counts"), r.get("document", {}).get("counts"))
    same(f"#{i} inspect members", [(m["name"], m["size"], m["method"]) for m in p["members"]],
         [(m["name"], m["size"], m["method"]) for m in r["members"]])


def edits(i: int, src: Path, tmp: Path) -> None:
    s = str(src)
    work = tmp / f"x{i}"
    out = tmp / f"rt{i}.dsd"
    rs("extract", s, str(work))
    rs("pack", str(work), s, "-o", str(out))
    same(f"#{i} extract/pack byte-identical", out.read_bytes(), src.read_bytes())

    tables = envelope(rs("table", s, *[str(n) for n in range(1, 400)][: envelope(rs("inspect", s))["result"]["document"]["counts"].get("TABLE", 0)]))
    ops = []
    for t in (tables["result"]["tables"] if tables["ok"] else []):
        for row in t["rows"]:
            for cell in row["cells"]:
                if cell.get("tag") == "TD" and not cell.get("mixedContent") and cell.get("text", "").strip():
                    ops.append(("set-cell", t["index"], row["row"], cell["col"]))
                    break
            if row["acopy"] != "N":
                ops.append(("copy-row", t["index"], row["row"]))
            if row["adelete"] != "N":
                ops.append(("delete-row", t["index"], row["row"]))
    picked = ops[:: max(1, len(ops) // 12)][:12]
    for k, op in enumerate(picked):
        kind, t, r, *rest = op
        args = ["--table", str(t), "--row", str(r)]
        if kind == "set-cell":
            args += ["--col", str(rest[0]), "--text", "-1,234,567"]
        elif kind == "copy-row":
            args += ["--clear", "--count", "2"]
        a, b = tmp / f"p{i}-{k}.dsd", tmp / f"r{i}-{k}.dsd"
        pa, rb = py(kind, s, "-o", str(a), *args), rs(kind, s, "-o", str(b), *args)
        if not same(f"#{i} {kind} outcome", pa.returncode, rb.returncode) or pa.returncode:
            continue
        same(f"#{i} {kind} contents.xml", contents(a), contents(b))
        same(f"#{i} {kind} members", members(a), members(b))
        same(f"#{i} {kind} diff", text(py("diff", s, str(a))), text(rs("diff", s, str(b), "--format", "text")))
        same(f"#{i} {kind} diff --raw", text(py("diff", s, str(a), "--raw")), text(rs("diff", s, str(b), "--raw", "--format", "text")))

    rows = [r for r in envelope(rs("read", s))["result"]["rows"] if len(r.get("text", "").strip()) >= 2]
    for k, row in enumerate(rows[:: max(1, len(rows) // 5)][:5]):
        old = row["text"].strip()[:2]
        p = py("replace", s, "--old", old, "--new", "X", "--dry-run")
        r = envelope(rs("replace", s, "--old", old, "--new", "X", "--dry-run"))
        count = int(text(p).split(" ", 1)[0])
        same(f"#{i} replace dry-run count", count, r["result"]["count"] if r["ok"] else None)
        a, b = tmp / f"rp{i}-{k}.dsd", tmp / f"rr{i}-{k}.dsd"
        pa = py("replace", s, "-o", str(a), "--old", old, "--new", "변경&<", "--expect", "any")
        rb = rs("replace", s, "-o", str(b), "--old", old, "--new", "변경&<", "--expect", "any")
        if same(f"#{i} replace outcome", pa.returncode, rb.returncode) and not pa.returncode:
            same(f"#{i} replace contents.xml", contents(a), contents(b))


def main() -> int:
    with tempfile.TemporaryDirectory() as td:
        tmp = Path(td)
        for i, src in enumerate(SAMPLES):
            if not src.is_file():
                continue
            before = len(failures)
            views(i, src)
            checks(i, src)
            edits(i, src, tmp)
            print(f"{'ok  ' if len(failures) == before else 'FAIL'} sample #{i}")
    print(f"\n{len(failures)} difference(s)" if failures else "\nparity: all comparisons equal")
    return 1 if failures else 0


if __name__ == "__main__":
    sys.exit(main())
