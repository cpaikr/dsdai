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
import xml.etree.ElementTree as ET
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


def view_rows(path: Path, scaffold: bool = False) -> list:
    """(line, path, rest) for each line of `read`'s text view."""
    view = dsd.TextView(dsd.Doc(dsd.read_archive(path).contents), scaffold=scaffold)
    view.render()
    return view.rows


def visible_text(path: Path) -> str:
    """All document text the view shows, without whitespace or inline markers."""
    shown = "".join(json.loads(rest) for _, _, rest in view_rows(path) if rest.startswith('"'))
    return re.sub(r"[ \t\r\n]|⟦[^⟧]*⟧", "", shown)


def source_text(path: Path) -> str:
    """All text in contents.xml outside tables and editor scaffolding, without whitespace."""
    def walk(node: dsd.Node) -> str:
        if node.tag in dsd.SCAFFOLD_TAGS or node.tag == "TABLE":
            return ""
        return "".join(i if isinstance(i, str) else walk(i) for i in node.items)

    text = walk(dsd.Doc(dsd.read_archive(path).contents).root)
    return re.sub(r"[ \t\r\n]", "", text.replace(dsd.CR_TOKEN, dsd.LINE_BREAK_MARK))


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

    def test_text_view(self) -> None:
        code, out, err = run("read", str(self.src))
        self.assertEqual(code, 0, err)
        self.assertIn('# schema: "dart4.xsd"\n', out)
        self.assertNotIn("# unverified", out)
        self.assertRegex(out, r"\nL25 +COVER/COVER-TITLE +\"감   사   보   고   서\"\n")  # spacing kept exactly
        self.assertNotIn("홍길동", out)  # cell text lives in `table`, not in the view
        self.assertNotIn("◆click◆", out)  # editor scaffolding is hidden by default
        rows = view_rows(self.src)
        for row in (
            ("SECTION-1/P", r'"우리는 가나다테크 주식회사(이하 \"회사\")의 재무제표를 감사하였습니다.⏎둘째 줄입니다."'),
            ("COVER/P", '"⏎⏎"'),
            ("SECTION-2/TITLE", '"재무상태표"'),  # paths start at the innermost section
            ("SECTION-2/TABLE", "T4 rows=4 cols=3 -> table 4"),
            ("SECTION-2/IMAGE/IMG", '"대표이사 확인서.jpg"'),
            ("COVER/PGBRK", ""),
            ("DOCUMENT-HEADER/SUMMARY/EXTRACTION[ACODE=FIN_STAT]", '"100"'),
        ):
            self.assertIn(row, [r[1:] for r in rows])
        line = next(r[0] for r in rows if r[1] == "SECTION-2/TITLE")
        self.assertIn('<TITLE ATOC="Y" ATOCID="4">', contents_of(self.src).splitlines()[line - 1])

        shown = [r[1:] for r in view_rows(self.src, scaffold=True)]
        self.assertIn(("INSERTION/COMMENT", '"◆click◆ 예시: 정정 안내 문단 삽입"'), shown)
        self.assertIn(("INSERTION/LIBRARY/FILENAME", '"01_예시 감사인 보고서"'), shown)
        code, out, _ = run("read", str(self.src), "--scaffold", "--no-header")
        self.assertTrue(out.startswith("L7 "), out[:40])
        self.assertIn("◆click◆", out)

        txt = self.dir / "sample.txt"
        code, _, err = run("read", str(self.src), "-o", str(txt))
        self.assertEqual(code, 0, err)
        self.assertIn("# legend:", txt.read_text(encoding="utf-8"))
        self.assertEqual(sorted(p.name for p in self.dir.iterdir()), ["sample.dsd", "sample.txt"])  # no image assets
        code, _, err = run("read", str(self.src), "-o", str(txt))
        self.assertEqual(code, 1)
        self.assertIn("already exists", err)

    def test_text_view_shows_elements_it_has_no_rule_for(self) -> None:
        xml = contents_of(self.src)
        for old, new in (
            ("<P>1. 일반사항</P>", '<NEWTHING AFLAG="Y" ANOTE="a b"><P>1. 일반사항</P>설명</NEWTHING>'),
            ('<P USERMARK="B">감사의견</P>', '<P USERMARK="B">감사<EMPHASIS AKIND="1">의견</EMPHASIS></P>'),
            ("<WARNING>", "<CAUTION>주의 문구</CAUTION><WARNING>"),
            ('ATOCID="4">재무상태표</TITLE>', 'ATOCID="4">재무상태표</TITLE>떠도는 글'),
            (">Ⅰ.유동자산</TD>", "><EMPHASIS>Ⅰ.유동자산</EMPHASIS></TD>"),
        ):
            self.assertEqual(xml.count(old), 1, old)
            xml = xml.replace(old, new)
        variant = self._with_contents(xml, "unknown.dsd")
        rows = [r[1:] for r in view_rows(variant)]
        note = 'SECTION-2/NEWTHING[AFLAG=Y ANOTE="a b"]'
        at = rows.index((note, ""))
        self.assertEqual(rows[at + 1:at + 3], [(note + "/P", '"1. 일반사항"'), (note + "/#text", '"설명"')])
        self.assertIn(("SECTION-1/P", '"감사⟦EMPHASIS[AKIND=1]⟧의견⟦/EMPHASIS⟧"'), rows)
        self.assertIn(("SECTION-1/CAUTION", '"주의 문구"'), rows)
        self.assertIn(("SECTION-2/#text", '"떠도는 글"'), rows)
        self.assertIn(("SECTION-2/TABLE", "T3 rows=6 cols=3 -> table 3 [contains unverified EMPHASIS]"), rows)
        code, out, _ = run("read", str(variant))
        self.assertIn('# unverified: ["CAUTION", "EMPHASIS", "NEWTHING"]', out)
        self.assertEqual(visible_text(variant), source_text(variant))

    def test_text_view_keeps_all_text_outside_tables(self) -> None:
        self.assertEqual(visible_text(self.src), source_text(self.src))

    def test_text_view_edge_layouts(self) -> None:
        xml = contents_of(self.src)
        for old, new in (
            # Stray text on its own CRLF line, and stray text that is only U+3000.
            ('ATOCID="4">재무상태표</TITLE>\r\n', 'ATOCID="4">재무상태표</TITLE>\r\n\r\n떠도는 글\r\n'),
            ('ATOCID="3">(첨부)재 무 제 표</TITLE>', 'ATOCID="3">(첨부)재 무 제 표</TITLE>\u3000\u3000'),
            # A table inside a paragraph keeps its T# stub; scaffolding does not split a generic element.
            ("<P>1. 일반사항</P>", "<P>1. 일반사항<TABLE><TBODY><TR><TD>셀</TD></TR></TBODY></TABLE>끝</P>"
                                 "<NEWTHING>앞<COMMENT>안내</COMMENT>뒤</NEWTHING>"),
        ):
            self.assertEqual(xml.count(old), 1, old)
            xml = xml.replace(old, new)
        variant = self._with_contents(xml, "edges.dsd")
        rows = view_rows(variant)
        stray = next(r for r in rows if r[2] == '"떠도는 글"')
        self.assertEqual(contents_of(variant).split("\r\n")[stray[0] - 1], "떠도는 글")
        self.assertIn(("SECTION-1/#text", '"\u3000\u3000"'), [r[1:] for r in rows])
        self.assertIn(("SECTION-2/P", '"1. 일반사항⟦T4 -> table 4⟧끝"'), [r[1:] for r in rows])
        self.assertIn(("SECTION-2/NEWTHING", '"앞뒤"'), [r[1:] for r in rows])
        self.assertEqual(visible_text(variant), source_text(variant))

    def test_text_view_rules_cover_only_verified_elements(self) -> None:
        ruled = dsd.TEXT_VIEW_TAGS | dsd.CONTAINER_TAGS | dsd.INLINE_TAGS | {"TABLE", "PGBRK"}
        self.assertLessEqual(ruled, dsd.VERIFIED_TAGS)

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

    def test_set_cell_refuses_span_covered_coordinates(self) -> None:
        # T4 R1C3 lies under the COLSPAN="2" header anchored at R1C2.
        code, _, err = run("set-cell", str(self.src), "--table", "4", "--row", "1", "--col", "3",
                           "--text", "X", "-o", str(self.out()))
        self.assertEqual(code, 1)
        self.assertIn("covered by the spanning cell at R1C2", err)
        self.assertFalse(self.out().exists())
        code, _, err = run("set-cell", str(self.src), "--table", "4", "--row", "1", "--col", "2",
                           "--text", "X", "-o", str(self.out()))
        self.assertEqual(code, 0, err)

    def test_check_warns_when_period_copies_disagree(self) -> None:
        repeat = (
            '<TU ALIGN="RIGHT" AUNIT="PERIODTO2" AUNITVALUE="20251231" HEIGHT="23" WIDTH="333">'
            "2025년 12월 31일</TU>"
        )
        xml = contents_of(self.src).replace("<TD HEIGHT=\"23\" WIDTH=\"249\">까지</TD>", repeat, 1)
        self.assertIn("PERIODTO2", xml)
        variant = self._with_contents(xml)
        code, out, _ = run("check", str(variant), "--no-schema")
        self.assertNotIn("period-mismatch", out)
        # Moving only the first copy leaves the repeat stale.
        code, out, err = run("set-cell", str(variant), "--table", "1", "--row", "2", "--col", "1",
                             "--text", "2026년 12월 31일", "--unit-value", "20261231", "-o", str(self.out()))
        self.assertEqual(code, 0, err)
        self.assertIn("period-mismatch", out)
        code, out, _ = run("check", str(self.out()), "--no-schema")
        self.assertIn("PERIODTO/PERIODTO2 TU cells disagree: T1R2C1=20261231, T1R2C2=20251231", out)

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

    def test_diff_reports_inserted_row_at_its_coordinates(self) -> None:
        # An empty last cell next to a long unchanged tail used to pair with the
        # inserted row's empty last cell and shift the report one cell early.
        xml = contents_of(self.src).replace(
            'WIDTH="176">500,000</TD>\r\n</TR>\r\n\r\n<TR ACOPY="N"', 'WIDTH="176"></TD>\r\n</TR>\r\n\r\n<TR ACOPY="N"', 1)
        tail = "".join(f"<P>꼬리 {i}</P>\r\n" for i in range(40))
        xml = xml.replace("<PGBRK></PGBRK>\r\n</SECTION-2>", tail + "<PGBRK></PGBRK>\r\n</SECTION-2>", 1)
        base = self._with_contents(xml)
        edits = self.dir / "edits.json"
        edits.write_text(json.dumps([
            {"op": "copy-row", "table": 3, "row": 5, "clear": True},
            {"op": "set-cell", "table": 3, "row": 6, "col": 1, "text": "   테스트"},
            {"op": "set-cell", "table": 3, "row": 6, "col": 2, "text": "(1,000)"},
        ], ensure_ascii=False), encoding="utf-8")
        code, _, err = run("apply", str(base), str(edits), "-o", str(self.out()))
        self.assertEqual(code, 0, err)
        code, diff, _ = run("diff", str(base), str(self.out()))
        changes = [line for line in diff.splitlines() if line[:2] in ("+ ", "- ", "~ ")]
        self.assertEqual(changes, ['+ T3R6C1 TD "   테스트"', '+ T3R6C2 TD "(1,000)"', '+ T3R6C3 TD ""'])
        # Deleting it again is reported as the same whole row, and in-place
        # edits still pair cell by cell.
        code, _, err = run("delete-row", str(self.out()), "--table", "3", "--row", "6", "-o", str(self.out("b.dsd")))
        self.assertEqual(code, 0, err)
        code, diff, _ = run("diff", str(self.out()), str(self.out("b.dsd")))
        self.assertEqual([line[:9] for line in diff.splitlines() if line.startswith("- ")], ["- T3R6C1 ", "- T3R6C2 ", "- T3R6C3 "])
        code, _, err = run("set-cell", str(base), "--table", "3", "--row", "3", "--col", "2",
                           "--text", "999", "-o", str(self.out("c.dsd")))
        self.assertEqual(code, 0, err)
        code, diff, _ = run("diff", str(base), str(self.out("c.dsd")))
        self.assertIn('~ T3R3C2 -> T3R3C2  "1,500,000" -> "999"', diff)
        self.assertIn("1 changed block(s)", diff)

    def test_diff_pairs_edited_row_next_to_inserted_row(self) -> None:
        # The edited row holds two equal cells, so cell matching alone can pair
        # the wrong one once a row is inserted beside it.
        def changes(edits: list) -> list:
            path = self.dir / "edits.json"
            path.write_text(json.dumps(edits, ensure_ascii=False), encoding="utf-8")
            out = self.out("adj.dsd")
            code, _, err = run("apply", str(self.src), str(path), "-o", str(out), "--force")
            self.assertEqual(code, 0, err)
            _, diff, _ = run("diff", str(self.src), str(out))
            return [line for line in diff.splitlines() if line[:2] in ("+ ", "- ", "~ ")]

        self.assertEqual(changes([
            {"op": "copy-row", "table": 3, "row": 4, "clear": True},
            {"op": "set-cell", "table": 3, "row": 5, "col": 1, "text": "   기타"},
            {"op": "set-cell", "table": 3, "row": 6, "col": 2, "text": "1,600,000"},
        ]), [
            '+ T3R5C1 TD "   기타"', '+ T3R5C2 TD ""', '+ T3R5C3 TD ""',
            '~ T3R5C2 -> T3R6C2  "500,000" -> "1,600,000"',
        ])
        # Filling a cell and inserting a row right after it stays two separate changes.
        self.assertEqual(changes([
            {"op": "set-cell", "table": 3, "row": 5, "col": 3, "text": "X"},
            {"op": "copy-row", "table": 3, "row": 5, "clear": True},
            {"op": "set-cell", "table": 3, "row": 6, "col": 1, "text": "x"},
        ]), [
            '~ T3R5C3 -> T3R5C3  "500,000" -> "X"',
            '+ T3R6C1 TD "x"', '+ T3R6C2 TD ""', '+ T3R6C3 TD ""',
        ])

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
        coverage = text.split("## Verified coverage", 1)[1]
        observed, rest = coverage.split("Verified by schema equivalence", 1)
        equivalent = rest.split("\n\n", 2)[1]  # the tag list paragraph after the intro
        names = re.compile(r"`([A-Za-z][A-Za-z0-9-]*)`")
        self.assertEqual(set(names.findall(observed)), set(dsd.OBSERVED_TAGS))
        self.assertEqual(set(names.findall(equivalent)), set(dsd.SCHEMA_EQUIVALENT_TAGS))
        self.assertEqual(dsd.VERIFIED_TAGS, dsd.OBSERVED_TAGS | dsd.SCHEMA_EQUIVALENT_TAGS)
        fixture_tags = set(re.findall(r"<([A-Za-z][A-Za-z0-9-]*)[\s>/]", contents_of(self.src)))
        self.assertLessEqual(fixture_tags, set(dsd.OBSERVED_TAGS))

    def test_schema_equivalent_tags_match_installed_schemas(self) -> None:
        # Reads the locally installed (private, uncommitted) DART schemas at run
        # time; no schema text or derived values live in the repository.
        schema_dir = HERE.parents[2] / "fixtures" / "private" / "dart-resource"
        schemas = sorted(schema_dir.glob("*.xsd")) if schema_dir.is_dir() else []
        if not schemas:
            self.skipTest("installed DART schemas absent (fixtures/private/)")
        xs = "{http://www.w3.org/2001/XMLSchema}"
        nested_section = re.compile(r"(?i)section-\d+$")
        pairs = {"SECTION-4": "SECTION-2", "SECTION-5": "SECTION-2", "SECTION-6": "SECTION-2",
                 "SECTION-7": "SECTION-2", "TFOOT": "TBODY"}
        self.assertEqual(set(pairs), set(dsd.SCHEMA_EQUIVALENT_TAGS))
        for path in schemas:
            root = ET.parse(path).getroot()
            types = {t.get("name"): t for t in root.iter(xs + "complexType") if t.get("name")}
            element_type: dict = {}
            for e in root.iter(xs + "element"):
                if e.get("name") and e.get("type"):
                    element_type.setdefault(e.get("name"), e.get("type"))

            def shape(el: ET.Element, top: bool = True) -> object:
                # Structure and attributes, ignoring annotations, the type's own
                # name, and the nested section level (its number, or its absence
                # at the deepest level).
                if el.tag == xs + "annotation":
                    return None
                ident = el.get("name") or el.get("ref") or ""
                if not top and el.tag == xs + "element" and nested_section.match(ident):
                    return None
                attrs = tuple(sorted((k, v) for k, v in el.attrib.items() if not (top and k == "name")))
                kids = tuple(s for s in (shape(c, False) for c in el) if s is not None)
                return (el.tag, attrs, kids)

            for tag, base in pairs.items():
                with self.subTest(schema=path.name, tag=tag):
                    self.assertEqual(shape(types[element_type[tag]]), shape(types[element_type[base]]))

    def test_readers_flag_unverified_elements(self) -> None:
        variant = self._note_variant()
        code, out, _ = run("check", str(variant), "--no-schema")
        self.assertEqual(code, 0, out)  # a warning, not an error
        self.assertIn("unverified-element", out)
        self.assertIn("<NOTE> x1", out)
        code, out, _ = run("read", str(variant))
        self.assertEqual(code, 0)
        self.assertIn('# unverified: ["NOTE"]', out)
        self.assertRegex(out, r"\nL\d+ +SECTION-2/NOTE\nL\d+ +SECTION-2/NOTE/P +\"1\. 일반사항\"\n")
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

    def _renamed_section(self, level: int) -> Path:
        xml = contents_of(self.src).replace("<SECTION-2 ", f"<SECTION-{level} ").replace(
            "</SECTION-2>", f"</SECTION-{level}>")
        return self._with_contents(xml, f"section{level}.dsd")

    def test_unverified_section_keeps_its_outline_title(self) -> None:
        # No schema declares SECTION-8; it stands in for any unverified level.
        code, out, _ = run("outline", str(self._renamed_section(8)))
        self.assertEqual(code, 0)
        self.assertRegex(out, r"SECTION-8 재무상태표 \[unverified element\]")

    def test_schema_equivalent_elements_are_editable(self) -> None:
        deep = self._renamed_section(4)
        code, out, _ = run("check", str(deep), "--no-schema")
        self.assertIn("ok (no issues)", out)
        code, out, _ = run("outline", str(deep))
        self.assertRegex(out, r"SECTION-4 재무상태표\r?\n")
        code, out, _ = run("read", str(deep))
        self.assertRegex(out, r"\nL\d+ +SECTION-4/TITLE +\"재무상태표\"\n")
        self.assertNotIn("unverified", out)
        code, _, err = run("set-cell", str(deep), "--table", "3", "--row", "4", "--col", "2",
                           "--text", "1,100,000", "-o", str(self.out("a.dsd")))
        self.assertEqual(code, 0, err)
        code, _, err = run("replace", str(deep), "--old", "1. 일반사항", "--new", "1. 일반 사항",
                           "-o", str(self.out("b.dsd")))
        self.assertEqual(code, 0, err)

        xml = contents_of(self.src)
        self.assertIn("</TBODY>", xml)
        footed = self._with_contents(xml.replace("</TBODY>", "</TBODY>\r\n<TFOOT></TFOOT>", 1), "tfoot.dsd")
        code, out, _ = run("check", str(footed), "--no-schema")
        self.assertIn("ok (no issues)", out)

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
        self.assertIn('# unverified: ["schema dart9.xsd"]', out)
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
