"""Unit tests for scripts/dsd.py against the synthetic fixture.

Run: python -m unittest discover -s skills/dart-dsd/evals -p "test_*.py"
"""

from __future__ import annotations

import contextlib
import io
import json
import re
import sys
import tempfile
import unittest
import zipfile
from pathlib import Path

HERE = Path(__file__).resolve().parent
sys.path.insert(0, str(HERE.parent / "scripts"))
sys.path.insert(0, str(HERE))

import dsd  # noqa: E402
import make_fixture  # noqa: E402


def run(*argv: str) -> tuple:
    out, err = io.StringIO(), io.StringIO()
    with contextlib.redirect_stdout(out), contextlib.redirect_stderr(err):
        code = dsd.main(list(argv))
    return code, out.getvalue(), err.getvalue()


def contents_of(path: Path) -> str:
    return zipfile.ZipFile(path).read("contents.xml").decode("utf-8")


class DsdTestCase(unittest.TestCase):
    def setUp(self) -> None:
        self.tmp = tempfile.TemporaryDirectory()
        self.dir = Path(self.tmp.name)
        self.src = self.dir / "sample.dsd"
        self.src.write_bytes(make_fixture.fixture_bytes())

    def tearDown(self) -> None:
        self.tmp.cleanup()

    def out(self, name: str = "out.dsd") -> Path:
        return self.dir / name

    def _with_contents(self, xml: str, name: str = "variant.dsd") -> Path:
        path = self.dir / name
        path.write_bytes(make_fixture.build_zip([
            (b"contents.xml", xml.encode("utf-8")),
            (b"meta.xml", make_fixture.crlf(make_fixture.META)),
            (make_fixture.IMAGE_NAME.encode("cp949"), make_fixture.IMAGE_BYTES),
        ]))
        return path


class ReadTests(DsdTestCase):
    def test_check_and_inspect_are_clean(self) -> None:
        code, out, _ = run("check", str(self.src), "--no-schema")
        self.assertEqual(code, 0, out)
        self.assertIn("ok (no issues)", out)
        code, out, _ = run("inspect", str(self.src), "--json")
        report = json.loads(out)
        image = report["members"][2]
        self.assertEqual(image["name"], make_fixture.IMAGE_NAME)
        self.assertEqual(image["name_encoding"], "cp949")
        self.assertFalse(image["utf8_flag"])
        self.assertEqual(report["document"]["line_endings"], "CRLF")

    def test_markdown_rendering(self) -> None:
        md = self.dir / "sample.md"
        code, _, err = run("read", str(self.src), "-o", str(md))
        self.assertEqual(code, 0, err)
        text = md.read_text(encoding="utf-8")
        self.assertIn("# 감 사 보 고 서", text)
        self.assertIn("## 독립된 감사인의 감사보고서", text)
        self.assertIn("### 재무상태표", text)
        self.assertIn("재무제표를 감사하였습니다.  \n둘째 줄입니다.", text)  # &cr; -> line break
        self.assertIn('<th rowspan="2">주주명</th><th colspan="2">당기말</th>', text)
        self.assertIn("| 현금및현금성자산 | 1,000,000 | 700,000 |", text)
        self.assertIn("첫 문단<br>둘째 문단", text)
        self.assertNotIn("◆click◆", text)  # editor scaffolding is not content
        self.assertIn("(<sample.assets/image-001.jpg>)", text)
        self.assertEqual((self.dir / "sample.assets" / "image-001.jpg").read_bytes(), make_fixture.IMAGE_BYTES)

    def test_find_is_whitespace_insensitive(self) -> None:
        code, out, _ = run("find", str(self.src), "자산총계")
        self.assertEqual(code, 0)
        self.assertIn("T3R6C1", out)
        code, _, _ = run("find", str(self.src), "자산총계", "--exact")
        self.assertEqual(code, 1)

    def test_table_coordinates_follow_spans(self) -> None:
        code, out, _ = run("table", str(self.src), "4")
        self.assertEqual(code, 0)
        self.assertIn("C1 ^ covered by R1C1", out)
        self.assertIn('C2 TE "60,000" ACODE=SH5_NM', out)


class EditTests(DsdTestCase):
    def test_set_cell_preserves_everything_else(self) -> None:
        out = self.out()
        code, stdout, err = run("set-cell", str(self.src), "--table", "3", "--row", "4", "--col", "2",
                                "--text", "1,100,000", "-o", str(out))
        self.assertEqual(code, 0, err)
        before, after = contents_of(self.src), contents_of(out)
        self.assertEqual(after, before.replace(">1,000,000</TD>", ">1,100,000</TD>", 1))
        src_zip, out_zip = zipfile.ZipFile(self.src), zipfile.ZipFile(out)
        for a, b in zip(src_zip.infolist(), out_zip.infolist()):
            self.assertEqual((a.orig_filename, a.flag_bits, a.date_time), (b.orig_filename, b.flag_bits, b.date_time))
        self.assertEqual(src_zip.read(src_zip.infolist()[2]), out_zip.read(out_zip.infolist()[2]))

    def test_set_cell_keeps_paragraph_wrapper_and_escapes(self) -> None:
        out = self.out()
        code, _, err = run("set-cell", str(self.src), "--table", "2", "--row", "1", "--col", "1",
                           "--text", 'A & "B"\n<C>', "-o", str(out))
        self.assertEqual(code, 0, err)
        self.assertIn("<P>A &amp; &quot;B&quot;&amp;cr;&lt;C&gt;</P>", contents_of(out))

    def test_set_cell_refuses_mixed_content_unless_flattened(self) -> None:
        code, _, err = run("set-cell", str(self.src), "--table", "2", "--row", "1", "--col", "2",
                           "--text", "x", "-o", str(self.out()))
        self.assertEqual(code, 1)
        self.assertIn("--flatten", err)
        self.assertFalse(self.out().exists())
        code, _, err = run("set-cell", str(self.src), "--table", "2", "--row", "1", "--col", "2",
                           "--text", "x", "--flatten", "-o", str(self.out()))
        self.assertEqual(code, 0, err)

    def test_tu_cell_requires_unit_value(self) -> None:
        args = ["set-cell", str(self.src), "--table", "1", "--row", "2", "--col", "1", "--text", "2026년 12월 31일"]
        code, _, err = run(*args, "-o", str(self.out()))
        self.assertEqual(code, 1)
        self.assertIn("AUNITVALUE='20251231'", err)
        code, _, err = run(*args, "--unit-value", "20261230", "-o", str(self.out("a.dsd")))
        self.assertEqual(code, 1)
        self.assertIn("make them agree", err)
        self.assertFalse(self.out("a.dsd").exists())
        code, out, err = run(*args, "--unit-value", "20261231", "-o", str(self.out("b.dsd")))
        self.assertEqual(code, 0, err)
        self.assertIn('AUNITVALUE="20261231"', contents_of(self.out("b.dsd")))
        code, diff, _ = run("diff", str(self.src), str(self.out("b.dsd")))
        self.assertIn('@AUNITVALUE "20251231" -> "20261231"', diff)

    def test_set_cell_fills_empty_and_self_closing_wrappers(self) -> None:
        a = self.out("a.dsd")
        run("copy-row", str(self.src), "--table", "4", "--row", "3", "--clear", "-o", str(a))
        code, _, err = run("set-cell", str(a), "--table", "4", "--row", "4", "--col", "1", "--text", "김철수",
                           "-o", str(self.out("b.dsd")))
        self.assertEqual(code, 0, err)
        self.assertIn('WIDTH="191">김철수</TD>', contents_of(self.out("b.dsd")))
        bad = self._with_contents(contents_of(self.src).replace("<P>가나다테크 주식회사</P>", '<P USERMARK="B"/>'))
        code, _, err = run("set-cell", str(bad), "--table", "2", "--row", "1", "--col", "1", "--text", "NEW",
                           "-o", str(self.out("c.dsd")))
        self.assertEqual(code, 0, err)
        self.assertIn('<P USERMARK="B">NEW</P>', contents_of(self.out("c.dsd")))

    def test_negative_text_value_is_accepted(self) -> None:
        code, _, err = run("set-cell", str(self.src), "--table", "3", "--row", "4", "--col", "3",
                           "--text", "-700,000", "-o", str(self.out()))
        self.assertEqual(code, 0, err)
        self.assertIn(">-700,000</TD>", contents_of(self.out()))

    def test_replace_guards_count_and_skips_markup(self) -> None:
        code, _, err = run("replace", str(self.src), "--old", "1,500,000", "--new", "1,600,000", "-o", str(self.out()))
        self.assertEqual(code, 1)
        self.assertIn("found 2 occurrence(s)", err)
        code, _, err = run("replace", str(self.src), "--old", "1,500,000", "--new", "1,600,000",
                           "--expect", "2", "-o", str(self.out()))
        self.assertEqual(code, 0, err)
        # Attribute values and tag names are never matched.
        code, _, err = run("replace", str(self.src), "--old", "NORMAL", "--new", "X", "-o", str(self.out("b.dsd")))
        self.assertEqual(code, 1)
        self.assertIn("not found", err)

    def test_replace_ignores_entities_scaffolding_and_comments(self) -> None:
        # "&" must match real ampersands only, never the &amp;cr; line-break token.
        code, _, err = run("replace", str(self.src), "--old", "&", "--new", "와", "--expect", "any", "-o", str(self.out()))
        self.assertEqual(code, 1)
        self.assertIn("not found", err)
        # Scaffolding text (COMMENT, LIBRARYLIST, FILENAME) is excluded: only the TITLE matches.
        code, out, _ = run("replace", str(self.src), "--old", "감사인", "--new", "X", "--dry-run")
        self.assertEqual(code, 0)
        self.assertIn("1 occurrence(s)", out)
        commented = contents_of(self.src).replace("<P>1. 일반사항</P>", "<P>1. 일반<!-- a > b 사항 -->사항</P>")
        variant = self._with_contents(commented)
        code, out, _ = run("replace", str(variant), "--old", "사항", "--new", "X", "--dry-run")
        self.assertIn("1 occurrence(s)", out)
        # A newline in --old means a DART line break and matches across &cr;.
        code, _, err = run("replace", str(self.src), "--old", "하였습니다.\n둘째", "--new", "하였습니다. 둘째",
                           "-o", str(self.out("b.dsd")))
        self.assertEqual(code, 0, err)
        self.assertIn("하였습니다. 둘째 줄입니다.", contents_of(self.out("b.dsd")))

    def test_preexisting_errors_do_not_block_unrelated_edits(self) -> None:
        broken = contents_of(self.src).replace('COLSPAN="2" HEIGHT="23"', 'COLSPAN="3" HEIGHT="23"')
        variant = self._with_contents(broken)
        code, out, err = run("copy-row", str(variant), "--table", "3", "--row", "4", "-o", str(self.out()))
        self.assertEqual(code, 0, err + out)

    def test_clear_empties_mixed_cells_and_unit_values(self) -> None:
        code, out, err = run("copy-row", str(self.src), "--table", "2", "--row", "1", "--clear", "-o", str(self.out()))
        self.assertEqual(code, 0, err)
        self.assertIn("structure kept", out)
        self.assertIn("<P></P>\r\n<P></P>", contents_of(self.out()))

    def test_apply_validates_field_types(self) -> None:
        edits = self.dir / "edits.json"
        for bad, needle in (
            ({"op": "set-cell", "table": 3, "row": 4, "col": 2, "text": None}, "missing field 'text'"),
            ({"op": "copy-row", "table": 3, "row": 4, "clear": "false"}, "true or false"),
            ({"op": "delete-row", "table": 3, "row": None}, "missing field 'row'"),
            ({"op": "set-cell", "table": 3, "row": 4, "col": 2, "txt": "1"}, "unknown field"),
            ({"op": "replace", "old": "a", "new": "b", "expect": "abc"}, "positive integer"),
        ):
            edits.write_text(json.dumps([bad]), encoding="utf-8")
            code, _, err = run("apply", str(self.src), str(edits), "-o", str(self.out()))
            self.assertEqual(code, 1, bad)
            self.assertIn(needle, err)

    def test_copy_then_delete_row_restores_bytes(self) -> None:
        a, b = self.out("a.dsd"), self.out("b.dsd")
        self.assertEqual(run("copy-row", str(self.src), "--table", "3", "--row", "4", "--clear", "-o", str(a))[0], 0)
        copied = contents_of(a)
        self.assertIn('<TD HEIGHT="23" VALIGN="MIDDLE" WIDTH="247"></TD>', copied)
        self.assertEqual(run("delete-row", str(a), "--table", "3", "--row", "5", "-o", str(b))[0], 0)
        self.assertEqual(contents_of(b), contents_of(self.src))

    def test_row_locks_and_rowspans_are_respected(self) -> None:
        code, _, err = run("delete-row", str(self.src), "--table", "3", "--row", "6", "-o", str(self.out()))
        self.assertEqual(code, 1)
        self.assertIn('ADELETE="N"', err)
        code, _, err = run("copy-row", str(self.src), "--table", "4", "--row", "1", "-o", str(self.out()))
        self.assertEqual(code, 1)
        self.assertIn("ROWSPAN", err)

    def test_apply_runs_edits_in_order(self) -> None:
        edits = self.dir / "edits.json"
        edits.write_text(json.dumps([
            {"op": "copy-row", "table": 4, "row": 3, "clear": True},
            {"op": "set-cell", "table": 4, "row": 4, "col": 1, "text": "김철수"},
            {"op": "replace", "old": "제 5 기", "new": "제 6 기", "table": 3},
        ], ensure_ascii=False), encoding="utf-8")
        code, out, err = run("apply", str(self.src), str(edits), "-o", str(self.out()))
        self.assertEqual(code, 0, err)
        code, diff, _ = run("diff", str(self.src), str(self.out()))
        self.assertIn('"제 5 기" -> "제 6 기"', diff)
        self.assertIn('"김철수"', diff)

    def test_output_guards(self) -> None:
        code, _, err = run("set-cell", str(self.src), "--table", "3", "--row", "4", "--col", "2",
                           "--text", "1", "-o", str(self.src))
        self.assertEqual(code, 1)
        self.assertIn("already exists", err)
        self.out().write_bytes(b"keep")
        code, _, _ = run("set-cell", str(self.src), "--table", "3", "--row", "4", "--col", "2",
                         "--text", "1", "-o", str(self.out()))
        self.assertEqual(code, 1)
        self.assertEqual(self.out().read_bytes(), b"keep")


class PackTests(DsdTestCase):
    def test_extract_pack_round_trip_is_byte_identical(self) -> None:
        work = self.dir / "work"
        self.assertEqual(run("extract", str(self.src), str(work))[0], 0)
        self.assertEqual(run("pack", str(work), str(self.src), "-o", str(self.out()))[0], 0)
        self.assertEqual(self.out().read_bytes(), self.src.read_bytes())

    def test_pack_refuses_structural_damage(self) -> None:
        work = self.dir / "work"
        run("extract", str(self.src), str(work))
        xml = (work / "contents.xml").read_bytes()
        (work / "contents.xml").write_bytes(xml.replace(b'COLSPAN="2"', b'COLSPAN="3"', 1))
        code, out, err = run("pack", str(work), str(self.src), "-o", str(self.out()))
        self.assertEqual(code, 1)
        self.assertIn("table-grid", out)
        self.assertFalse(self.out().exists())

    def test_pack_rejects_line_ending_conversion(self) -> None:
        work = self.dir / "work"
        run("extract", str(self.src), str(work))
        xml = (work / "contents.xml").read_bytes()
        (work / "contents.xml").write_bytes(xml.replace(b"\r\n", b"\n"))
        code, out, _ = run("pack", str(work), str(self.src), "-o", str(self.out()))
        self.assertEqual(code, 1)
        self.assertIn("line-endings", out)

    def test_zip_comments_and_extra_fields_survive(self) -> None:
        import zipfile as zf
        src = self.dir / "commented.dsd"
        with zf.ZipFile(self.src) as z, zf.ZipFile(src, "w", zf.ZIP_DEFLATED) as w:
            w.comment = b"archive note"
            for info in z.infolist()[:2]:
                info.comment = b"member note"
                w.writestr(info, z.read(info))
        code, _, err = run("set-cell", str(src), "--table", "3", "--row", "4", "--col", "2", "--text", "1",
                           "-o", str(self.out()))
        self.assertEqual(code, 0, err)
        with zf.ZipFile(self.out()) as z:
            self.assertEqual(z.comment, b"archive note")
            self.assertEqual([i.comment for i in z.infolist()], [b"member note", b"member note"])
            self.assertIsNone(z.testzip())

    def test_read_and_extract_guards(self) -> None:
        code, _, err = run("read", str(self.src), "-o", str(self.src), "--force")
        self.assertEqual(code, 1)
        self.assertIn("source", err)
        code, _, err = run("extract", str(self.src), str(self.src))
        self.assertEqual(code, 1)
        self.assertIn("not a directory", err)
        code, _, err = run("check", str(self.src), "--schema-dir", str(self.dir / "missing"))
        self.assertEqual(code, 1)
        self.assertIn("schema directory not found", err)

    def test_pack_rejects_bom(self) -> None:
        work = self.dir / "work"
        run("extract", str(self.src), str(work))
        (work / "contents.xml").write_bytes(b"\xef\xbb\xbf" + (work / "contents.xml").read_bytes())
        code, _, err = run("pack", str(work), str(self.src), "-o", str(self.out()))
        self.assertEqual(code, 1)
        self.assertIn("BOM", err)


class UnverifiedContentTests(DsdTestCase):
    """Elements and schemas never seen in real DART output are flagged, never guessed at."""

    def _note_variant(self) -> Path:
        xml = contents_of(self.src).replace("<P>1. 일반사항</P>", "<NOTE><P>1. 일반사항</P></NOTE>")
        return self._with_contents(xml, "note.dsd")

    def _emphasis_in_table(self) -> Path:
        cell = '<TD HEIGHT="23" VALIGN="MIDDLE" WIDTH="247">Ⅰ.유동자산</TD>'
        self.assertIn(cell, contents_of(self.src))
        xml = contents_of(self.src).replace(cell, cell.replace(">Ⅰ.유동자산<", "><EMPHASIS>Ⅰ.유동자산</EMPHASIS><"))
        return self._with_contents(xml, "emphasis.dsd")

    def test_observed_tags_match_format_reference(self) -> None:
        text = (HERE.parent / "references" / "format.md").read_text(encoding="utf-8")
        section = text.split("## Verified coverage", 1)[1].split("The DART schemas also allow", 1)[0]
        listed = set(re.findall(r"`([A-Za-z][A-Za-z0-9-]*)`", section))
        self.assertEqual(listed, set(dsd.OBSERVED_TAGS))
        fixture_tags = set(re.findall(r"<([A-Za-z][A-Za-z0-9-]*)[\s>/]", contents_of(self.src)))
        self.assertLessEqual(fixture_tags, set(dsd.OBSERVED_TAGS))

    def test_readers_flag_unverified_elements(self) -> None:
        variant = self._note_variant()
        code, out, _ = run("check", str(variant), "--no-schema")
        self.assertEqual(code, 0, out)  # a warning, not an error
        self.assertIn("unverified-element", out)
        self.assertIn("<NOTE> x1", out)
        code, out, _ = run("read", str(variant))
        self.assertEqual(code, 0)
        self.assertIn('unverified: ["NOTE"]', out)
        self.assertRegex(out, r"<!-- dsd:unverified NOTE line=\d+: rendering not verified -->\n\n1\. 일반사항")
        code, out, _ = run("find", str(variant), "일반사항")
        self.assertIn("[unverified NOTE]", out)
        code, out, _ = run("outline", str(variant), "--all")
        self.assertIn("<NOTE> unverified element", out)
        code, out, _ = run("table", str(self._emphasis_in_table()), "3")
        self.assertIn("warning: contains unverified <EMPHASIS>", out)

    def test_edits_refuse_unverified_targets(self) -> None:
        variant = self._note_variant()
        code, out, _ = run("replace", str(variant), "--old", "일반사항", "--new", "X", "--dry-run")
        self.assertEqual(code, 0)
        self.assertIn("[inside unverified <NOTE>]", out)
        code, _, err = run("replace", str(variant), "--old", "일반사항", "--new", "X", "-o", str(self.out()))
        self.assertEqual(code, 1)
        self.assertIn("not verified", err)
        self.assertIn("extract -> edit -> pack", err)
        self.assertFalse(self.out().exists())
        # Text outside the unverified element is still editable.
        code, _, err = run("replace", str(variant), "--old", "첫 문단", "--new", "첫째 문단", "-o", str(self.out()))
        self.assertEqual(code, 0, err)

        table_variant = self._emphasis_in_table()
        for argv in (
            ("set-cell", str(table_variant), "--table", "3", "--row", "4", "--col", "2", "--text", "1"),
            ("copy-row", str(table_variant), "--table", "3", "--row", "4"),
            ("delete-row", str(table_variant), "--table", "3", "--row", "4"),
        ):
            code, _, err = run(*argv, "-o", str(self.out("t.dsd")))
            self.assertEqual(code, 1, argv)
            self.assertIn("T3 contains <EMPHASIS>", err)
        # Other tables in the same document are unaffected.
        code, _, err = run("set-cell", str(table_variant), "--table", "4", "--row", "3", "--col", "1",
                           "--text", "X", "-o", str(self.out("t.dsd")))
        self.assertEqual(code, 0, err)

    def test_edits_refuse_tables_inside_unverified_containers(self) -> None:
        xml = contents_of(self.src)
        tail = "</TABLE>\r\n\r\n<IMAGE>"
        self.assertEqual(xml.count(tail), 1)
        xml = xml.replace("<P>1. 일반사항</P>", "<P>1. 일반사항</P><NOTE>").replace(tail, "</TABLE></NOTE>\r\n\r\n<IMAGE>")
        variant = self._with_contents(xml, "note-table.dsd")
        for argv in (
            ("set-cell", str(variant), "--table", "4", "--row", "3", "--col", "1", "--text", "X"),
            ("copy-row", str(variant), "--table", "4", "--row", "3"),
            ("delete-row", str(variant), "--table", "4", "--row", "3"),
            ("replace", str(variant), "--old", "홍길동", "--new", "X"),
            ("replace", str(variant), "--old", "홍길동", "--new", "X", "--table", "4"),
        ):
            code, _, err = run(*argv, "-o", str(self.out("n.dsd")))
            self.assertEqual(code, 1, argv)
            self.assertIn("not verified", err)
        edits = self.dir / "edits.json"
        edits.write_text(json.dumps([{"op": "replace", "old": "홍길동", "new": "X", "table": 4}]), encoding="utf-8")
        code, _, err = run("apply", str(variant), str(edits), "-o", str(self.out("n.dsd")))
        self.assertEqual(code, 1)
        self.assertIn("not verified", err)
        self.assertFalse(self.out("n.dsd").exists())
        code, out, _ = run("replace", str(variant), "--old", "홍길동", "--new", "X", "--table", "4", "--dry-run")
        self.assertIn("[inside unverified <NOTE>]", out)
        code, out, _ = run("table", str(variant), "4")
        self.assertIn("warning: lies inside unverified <NOTE>", out)

    def test_replace_refuses_unverified_before_counting(self) -> None:
        code, _, err = run("replace", str(self._note_variant()), "--old", "일반사항", "--new", "X",
                           "--expect", "3", "-o", str(self.out()))
        self.assertEqual(code, 1)
        self.assertIn("not verified", err)
        self.assertNotIn("expected 3", err)

    def test_unverified_section_keeps_its_outline_title(self) -> None:
        xml = contents_of(self.src).replace("<SECTION-2 ", "<SECTION-4 ").replace("</SECTION-2>", "</SECTION-4>")
        code, out, _ = run("outline", str(self._with_contents(xml, "deep.dsd")))
        self.assertEqual(code, 0)
        self.assertRegex(out, r"SECTION-4 재무상태표 \[unverified element\]")

    def test_unverified_elements_inside_scaffolding_are_ignored(self) -> None:
        comment = "<COMMENT>◆click◆ 예시: 정정 안내 문단 삽입</COMMENT>"
        self.assertIn(comment, contents_of(self.src))
        xml = contents_of(self.src).replace(comment, comment.replace("◆click◆", "<EMPHASIS>◆click◆</EMPHASIS>"))
        variant = self._with_contents(xml, "scaffold.dsd")
        code, out, _ = run("check", str(variant), "--no-schema")
        self.assertEqual(code, 0, out)
        self.assertIn("ok (no issues)", out)
        code, out, _ = run("read", str(variant))
        self.assertNotIn("unverified", out)

    def test_unverified_schema_blocks_edits(self) -> None:
        xml = contents_of(self.src).replace('noNamespaceSchemaLocation="dart4.xsd"', 'noNamespaceSchemaLocation="dart9.xsd"')
        variant = self._with_contents(xml, "schema.dsd")
        code, out, _ = run("check", str(variant), "--no-schema")
        self.assertIn("unverified-schema", out)
        code, out, _ = run("read", str(variant))
        self.assertIn('unverified: ["schema dart9.xsd"]', out)
        code, _, err = run("set-cell", str(variant), "--table", "3", "--row", "4", "--col", "2",
                           "--text", "1", "-o", str(self.out()))
        self.assertEqual(code, 1)
        self.assertIn("schema dart9.xsd", err)
        code, _, err = run("replace", str(variant), "--old", "첫 문단", "--new", "X", "-o", str(self.out()))
        self.assertEqual(code, 1)
        self.assertIn("schema dart9.xsd", err)

    def test_raw_route_handles_unverified_elements(self) -> None:
        variant = self._note_variant()
        work = self.dir / "work"
        self.assertEqual(run("extract", str(variant), str(work))[0], 0)
        xml = (work / "contents.xml").read_bytes()
        (work / "contents.xml").write_bytes(xml.replace("<P>1. 일반사항</P>".encode(), "<P>1. 일반 사항</P>".encode()))
        code, out, err = run("pack", str(work), str(variant), "-o", str(self.out()))
        self.assertEqual(code, 0, err + out)
        self.assertIn("<NOTE><P>1. 일반 사항</P></NOTE>", contents_of(self.out()))
        code, out, _ = run("diff", str(variant), str(self.out()))
        self.assertIn("일반 사항", out)


class SafetyTests(DsdTestCase):
    def _zip(self, members: list) -> Path:
        path = self.dir / "bad.dsd"
        path.write_bytes(make_fixture.build_zip(members))
        return path

    def test_rejects_doctype(self) -> None:
        bad = self._zip([(b"contents.xml", b'<!DOCTYPE x [<!ENTITY a "b">]><DOCUMENT/>'), (b"meta.xml", b"<METAINFO/>")])
        code, out, _ = run("check", str(bad), "--no-schema")
        self.assertEqual(code, 1)
        self.assertIn("DOCTYPE", out)

    def test_rejects_path_traversal(self) -> None:
        bad = self._zip([(b"../contents.xml", b"<DOCUMENT/>")])
        code, _, err = run("inspect", str(bad))
        self.assertEqual(code, 1)
        self.assertIn("unsafe member path", err)

    def test_reports_missing_image(self) -> None:
        contents = make_fixture.crlf(make_fixture.CONTENTS)
        meta = make_fixture.crlf(make_fixture.META)
        bad = self._zip([(b"contents.xml", contents), (b"meta.xml", meta)])
        code, out, _ = run("check", str(bad), "--no-schema")
        self.assertEqual(code, 1)
        self.assertIn("image-missing", out)


if __name__ == "__main__":
    unittest.main()
