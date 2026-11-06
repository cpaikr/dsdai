//! The CLI as an external process, against the synthetic fixture.
#![allow(clippy::many_single_char_names, clippy::format_collect)]

mod support;

use serde_json::{Value, json};
use support::{Work, contents_of, dsd, replace_once};

fn s(p: &std::path::Path) -> String {
    p.display().to_string()
}

fn text_rows(result: &Value) -> Vec<(String, String)> {
    result["rows"]
        .as_array()
        .unwrap()
        .iter()
        .map(|r| (r["path"].as_str().unwrap().to_owned(), r["text"].as_str().unwrap_or("").to_owned()))
        .collect()
}

// ---------------------------------------------------------------------------
// process contract

#[test]
fn public_fixture_matches_its_sources() {
    let committed = std::fs::read(support::repo().join("fixtures/public/sample.dsd")).unwrap();
    let built = support::fixture_bytes();
    if std::env::var_os("DSD_UPDATE_FIXTURES").is_some() {
        std::fs::write(support::repo().join("fixtures/public/sample.dsd"), &built).unwrap();
        return;
    }
    assert!(committed == built, "fixtures/public/sample.dsd is stale; rerun with DSD_UPDATE_FIXTURES=1");
}

#[test]
fn bare_invocation_prints_root_help() {
    let bare = dsd::<_, &str>([]);
    let help = dsd(["--help"]);
    assert_eq!((bare.code, help.code), (0, 0));
    assert_eq!(bare.stdout, help.stdout);
    assert!(bare.stdout.starts_with("Read, check, and surgically edit"));
    assert!(bare.stdout.contains("set-cell"));
    assert!(bare.stderr.is_empty());
}

#[test]
fn version_is_plain_text_and_json() {
    let v = dsd(["--version"]);
    assert_eq!(v.stdout, format!("dsd {}\n", env!("CARGO_PKG_VERSION")));
    let r = dsd(["version"]).ok();
    assert_eq!(r["version"], env!("CARGO_PKG_VERSION"));
}

#[test]
fn invalid_invocations_exit_2_with_one_json_document() {
    for args in [vec!["nope"], vec!["set-cell"], vec!["find", "x.dsd", "a", "--limit", "0"]] {
        let run = dsd(&args);
        let e = run.fails("invalid_invocation");
        assert!(e["hint"].as_str().unwrap().contains("--help"), "{args:?}");
        assert_eq!(run.stdout.matches('\n').count(), 1, "one newline-terminated document");
        assert!(run.stderr.is_empty());
    }
}

#[test]
fn success_envelope_shape() {
    let w = Work::new("envelope");
    let run = dsd(["check", &s(&w.src()), "--no-schema"]);
    assert!(run.stdout.ends_with('\n') && run.stdout.matches('\n').count() == 1);
    assert!(run.stderr.is_empty());
    let v = run.json();
    assert_eq!(v["ok"], true);
    assert_eq!(v["command"], "check");
    assert_eq!(v["error"], Value::Null);
    assert_eq!(v["metadata"]["contract"], "1");
    assert!(v["warnings"].is_array());
    let pretty = dsd(["check", &s(&w.src()), "--no-schema", "--pretty"]);
    assert!(pretty.stdout.lines().count() > 3);
    assert_eq!(serde_json::from_str::<Value>(&pretty.stdout).unwrap(), v);
}

#[test]
fn missing_file_is_a_failure_envelope() {
    let w = Work::new("missing");
    dsd(["inspect", &s(&w.path("nope.dsd"))]).fails("file_not_found");
}

// ---------------------------------------------------------------------------
// reading

#[test]
fn check_and_inspect_are_clean() {
    let w = Work::new("clean");
    let r = dsd(["check", &s(&w.src()), "--no-schema"]).ok();
    assert_eq!(r["passed"], true);
    assert_eq!(r["issues"], json!([]));
    let r = dsd(["inspect", &s(&w.src())]).ok();
    let image = &r["members"][2];
    assert_eq!(image["name"], support::IMAGE_NAME);
    assert_eq!(image["nameEncoding"], "cp949");
    assert_eq!(image["utf8Flag"], false);
    assert_eq!(r["document"]["lineEndings"], "CRLF");
    assert_eq!(r["document"]["counts"]["TABLE"], 4);
}

#[test]
fn text_view() {
    let w = Work::new("view");
    let run = dsd(["read", &s(&w.src()), "--format", "text"]);
    assert_eq!(run.code, 0);
    let out = &run.stdout;
    assert!(out.contains("# schema: \"dart4.xsd\"\n"));
    assert!(!out.contains("# unverified"));
    assert!(out.contains("\nL25     COVER/COVER-TITLE"), "{out}");
    assert!(out.contains("\"감   사   보   고   서\"\n"), "spacing kept exactly");
    assert!(!out.contains("홍길동"), "cell text lives in `table`");
    assert!(!out.contains("◆click◆"), "scaffolding hidden by default");

    let r = dsd(["read", &s(&w.src())]).ok();
    let rows = text_rows(&r);
    for row in [
        ("SECTION-1/P", "우리는 가나다테크 주식회사(이하 \"회사\")의 재무제표를 감사하였습니다.\n둘째 줄입니다."),
        ("COVER/P", "\n\n"),
        ("SECTION-2/TITLE", "재무상태표"),
        ("SECTION-2/IMAGE/IMG", "대표이사 확인서.jpg"),
        ("COVER/PGBRK", ""),
        ("DOCUMENT-HEADER/SUMMARY/EXTRACTION[ACODE=FIN_STAT]", "100"),
    ] {
        assert!(rows.contains(&(row.0.to_owned(), row.1.to_owned())), "{row:?}");
    }
    let stub = r["rows"].as_array().unwrap().iter().find(|r| r["table"]["index"] == 4).unwrap();
    assert_eq!(stub["path"], "SECTION-2/TABLE");
    assert_eq!((stub["table"]["rows"].as_u64(), stub["table"]["cols"].as_u64()), (Some(4), Some(3)));
    let line = r["rows"].as_array().unwrap().iter().find(|r| r["path"] == "SECTION-2/TITLE").unwrap()["line"]
        .as_u64()
        .unwrap();
    let xml = w.contents();
    assert!(
        xml.split("\r\n").nth(usize::try_from(line).unwrap() - 1).unwrap().contains("<TITLE ATOC=\"Y\" ATOCID=\"4\">")
    );

    let rows = text_rows(&dsd(["read", &s(&w.src()), "--scaffold"]).ok());
    assert!(rows.contains(&("INSERTION/COMMENT".into(), "◆click◆ 예시: 정정 안내 문단 삽입".into())));
    assert!(rows.contains(&("INSERTION/LIBRARY/FILENAME".into(), "01_예시 감사인 보고서".into())));
    let run = dsd(["read", &s(&w.src()), "--scaffold", "--no-header", "--format", "text"]);
    assert!(run.stdout.starts_with("L7 "));

    let txt = w.path("sample.txt");
    let r = dsd(["read", &s(&w.src()), "-o", &s(&txt), "--format", "text"]).ok();
    assert_eq!(r["format"], "text");
    assert!(std::fs::read_to_string(&txt).unwrap().contains("# legend:"));
    let mut names: Vec<String> =
        std::fs::read_dir(&w.dir).unwrap().map(|e| e.unwrap().file_name().to_string_lossy().into_owned()).collect();
    names.sort();
    assert_eq!(names, ["sample.dsd", "sample.txt"], "no image assets");
    let run = dsd(["read", &s(&w.src()), "-o", &s(&txt)]);
    run.fails("output_exists");
}

#[test]
fn text_view_shows_elements_it_has_no_rule_for() {
    let w = Work::new("unknown");
    let mut xml = w.contents();
    for (old, new) in [
        ("<P>1. 일반사항</P>", "<NEWTHING AFLAG=\"Y\" ANOTE=\"a b\"><P>1. 일반사항</P>설명</NEWTHING>"),
        ("<P USERMARK=\"B\">감사의견</P>", "<P USERMARK=\"B\">감사<EMPHASIS AKIND=\"1\">의견</EMPHASIS></P>"),
        ("<WARNING>", "<CAUTION>주의 문구</CAUTION><WARNING>"),
        ("ATOCID=\"4\">재무상태표</TITLE>", "ATOCID=\"4\">재무상태표</TITLE>떠도는 글"),
        (">Ⅰ.유동자산</TD>", "><EMPHASIS>Ⅰ.유동자산</EMPHASIS></TD>"),
    ] {
        xml = replace_once(&xml, old, new);
    }
    let v = w.variant("unknown.dsd", &xml);
    let r = dsd(["read", &s(&v)]).ok();
    let rows = text_rows(&r);
    let note = "SECTION-2/NEWTHING[AFLAG=Y ANOTE=\"a b\"]";
    let at = rows.iter().position(|r| r == &(note.to_owned(), String::new())).unwrap();
    assert_eq!(rows[at + 1], (format!("{note}/P"), "1. 일반사항".into()));
    assert_eq!(rows[at + 2], (format!("{note}/#text"), "설명".into()));
    assert!(rows.contains(&("SECTION-1/P".into(), "감사⟦EMPHASIS[AKIND=1]⟧의견⟦/EMPHASIS⟧".into())));
    assert!(rows.contains(&("SECTION-1/CAUTION".into(), "주의 문구".into())));
    assert!(rows.contains(&("SECTION-2/#text".into(), "떠도는 글".into())));
    let stub = r["rows"].as_array().unwrap().iter().find(|r| r["table"]["index"] == 3).unwrap();
    assert_eq!(stub["table"]["unverified"], json!(["EMPHASIS"]));
    assert_eq!(r["unverified"], json!(["CAUTION", "EMPHASIS", "NEWTHING"]));
    let text = dsd(["read", &s(&v), "--format", "text"]).stdout;
    assert!(text.contains("# unverified: [\"CAUTION\", \"EMPHASIS\", \"NEWTHING\"]"));
    assert!(text.contains("T3 rows=6 cols=3 -> table 3 [contains unverified EMPHASIS]"));
}

#[test]
fn text_view_edge_layouts() {
    let w = Work::new("edges");
    let mut xml = w.contents();
    for (old, new) in [
        ("ATOCID=\"4\">재무상태표</TITLE>\r\n", "ATOCID=\"4\">재무상태표</TITLE>\r\n\r\n떠도는 글\r\n"),
        ("ATOCID=\"3\">(첨부)재 무 제 표</TITLE>", "ATOCID=\"3\">(첨부)재 무 제 표</TITLE>\u{3000}\u{3000}"),
        (
            "<P>1. 일반사항</P>",
            "<P>1. 일반사항<TABLE><TBODY><TR><TD>셀</TD></TR></TBODY></TABLE>끝</P><NEWTHING>앞<COMMENT>안내</COMMENT>뒤</NEWTHING>",
        ),
    ] {
        xml = replace_once(&xml, old, new);
    }
    let v = w.variant("edges.dsd", &xml);
    let r = dsd(["read", &s(&v)]).ok();
    let stray = r["rows"].as_array().unwrap().iter().find(|r| r["text"] == "떠도는 글").unwrap();
    let line = usize::try_from(stray["line"].as_u64().unwrap()).unwrap();
    assert_eq!(xml.split("\r\n").nth(line - 1).unwrap(), "떠도는 글");
    let rows = text_rows(&r);
    assert!(rows.contains(&("SECTION-1/#text".into(), "\u{3000}\u{3000}".into())));
    assert!(rows.contains(&("SECTION-1/TITLE".into(), "(첨부)재 무 제 표".into())));
    assert!(rows.contains(&("SECTION-2/P".into(), "1. 일반사항⟦T4 -> table 4⟧끝".into())));
    assert!(rows.contains(&("SECTION-2/NEWTHING".into(), "앞뒤".into())));
}

#[test]
fn find_is_whitespace_insensitive() {
    let w = Work::new("find");
    let r = dsd(["find", &s(&w.src()), "자산총계"]).ok();
    assert!(r["matches"].as_array().unwrap().iter().any(|m| m["locator"] == "T3R6C1"));
    let r = dsd(["find", &s(&w.src()), "자산총계", "--exact"]).ok();
    assert_eq!(r["matches"], json!([]));
    assert_eq!(dsd(["find", &s(&w.src()), "자산총계", "--exact", "--format", "text"]).stdout, "no matches\n");
    dsd(["find", &s(&w.src()), "  "]).fails("invalid_argument");
    assert_eq!(dsd(["find", &s(&w.src()), "- 1"]).ok()["query"], "- 1", "text may start with '-'");
}

#[test]
fn table_coordinates_follow_spans() {
    let w = Work::new("table");
    let out = dsd(["table", &s(&w.src()), "4", "--format", "text"]).stdout;
    assert!(out.contains("C1 ^ covered by R1C1"));
    assert!(out.contains("C2 TE \"60,000\" ACODE=SH5_NM"));
    let r = dsd(["table", &s(&w.src()), "4"]).ok();
    let t = &r["tables"][0];
    assert_eq!(t["rows"][0]["cells"][0]["rowspan"], 2);
    assert_eq!(t["rows"][1]["cells"][0]["coveredBy"], "R1C1");
    dsd(["table", &s(&w.src()), "9"]).fails("invalid_argument");
}

#[test]
fn outline_lists_structure() {
    let w = Work::new("outline");
    let r = dsd(["outline", &s(&w.src())]).ok();
    let entries = r["entries"].as_array().unwrap();
    assert!(entries.iter().any(|e| e["kind"] == "table" && e["index"] == 3 && e["rows"] == 6));
    assert!(entries.iter().any(|e| e["kind"] == "section" && e["title"] == "재무상태표" && e["depth"] == 1));
    let text = dsd(["outline", &s(&w.src()), "--format", "text"]).stdout;
    assert!(text.contains("  SECTION-2 재무상태표\n"));
}

// ---------------------------------------------------------------------------
// editing

fn set_cell(src: &std::path::Path, out: &std::path::Path, t: &str, r: &str, c: &str, text: &str) -> support::Run {
    dsd(["set-cell", &s(src), "-o", &s(out), "--table", t, "--row", r, "--col", c, "--text", text])
}

#[test]
fn set_cell_preserves_everything_else() {
    let w = Work::new("setcell");
    let out = w.path("out.dsd");
    let r = set_cell(&w.src(), &out, "3", "4", "2", "1,100,000").ok();
    assert_eq!(r["changes"][0]["op"], "set-cell");
    assert_eq!(r["sha256"].as_str().unwrap().len(), 64);
    assert_eq!(contents_of(&out), w.contents().replacen(">1,000,000</TD>", ">1,100,000</TD>", 1));
    let (a, b) = (dsd::read_archive(&w.src()).unwrap(), dsd::read_archive(&out).unwrap());
    for (x, y) in a.members.iter().zip(&b.members) {
        assert_eq!((&x.raw_name, x.flags, x.dos_time, x.dos_date), (&y.raw_name, y.flags, y.dos_time, y.dos_date));
    }
    assert_eq!(a.members[2].payload, b.members[2].payload, "untouched members keep their compressed bytes");
}

#[test]
fn set_cell_keeps_paragraph_wrapper_and_escapes() {
    let w = Work::new("escape");
    let out = w.path("out.dsd");
    set_cell(&w.src(), &out, "2", "1", "1", "A & \"B\"\n<C>").ok();
    assert!(contents_of(&out).contains("<P>A &amp; &quot;B&quot;&amp;cr;&lt;C&gt;</P>"));
}

#[test]
fn set_cell_refuses_mixed_content_unless_flattened() {
    let w = Work::new("mixed");
    let out = w.path("out.dsd");
    let run = set_cell(&w.src(), &out, "2", "1", "2", "x");
    run.fails("edit_refused");
    assert!(run.error_text().contains("--flatten"));
    assert!(!out.exists());
    dsd([
        "set-cell",
        &s(&w.src()),
        "-o",
        &s(&out),
        "--table",
        "2",
        "--row",
        "1",
        "--col",
        "2",
        "--text",
        "x",
        "--flatten",
    ])
    .ok();
}

#[test]
fn tu_cell_requires_unit_value() {
    let w = Work::new("tu");
    let base = |out: &std::path::Path| {
        vec![
            "set-cell".to_owned(),
            s(&w.src()),
            "-o".into(),
            s(out),
            "--table".into(),
            "1".into(),
            "--row".into(),
            "2".into(),
            "--col".into(),
            "1".into(),
            "--text".into(),
            "2026년 12월 31일".into(),
        ]
    };
    let run = dsd(base(&w.path("o.dsd")));
    run.fails("edit_refused");
    assert!(run.error_text().contains("AUNITVALUE=\"20251231\""), "{}", run.stdout);
    let mut args = base(&w.path("a.dsd"));
    args.extend(["--unit-value".into(), "20261230".into()]);
    let run = dsd(&args);
    run.fails("edit_refused");
    assert!(run.error_text().contains("text shows date 20261231 but AUNITVALUE is 20261230"));
    assert!(!w.path("a.dsd").exists());
    let mut args = base(&w.path("b.dsd"));
    args.extend(["--unit-value".into(), "20261231".into()]);
    dsd(&args).ok();
    assert!(contents_of(&w.path("b.dsd")).contains("AUNITVALUE=\"20261231\""));
    let diff = dsd(["diff", &s(&w.src()), &s(&w.path("b.dsd")), "--format", "text"]).stdout;
    assert!(diff.contains("@AUNITVALUE \"20251231\" -> \"20261231\""), "{diff}");
    let r = dsd(["diff", &s(&w.src()), &s(&w.path("b.dsd"))]).ok();
    assert_eq!(r["changes"][0]["attributes"][0], json!({"name": "AUNITVALUE", "old": "20251231", "new": "20261231"}));
}

#[test]
fn set_cell_refuses_span_covered_coordinates() {
    let w = Work::new("span");
    let run = set_cell(&w.src(), &w.path("o.dsd"), "4", "1", "3", "X");
    run.fails("edit_refused");
    assert!(run.error_text().contains("covered by the spanning cell at R1C2"));
    set_cell(&w.src(), &w.path("o.dsd"), "4", "1", "2", "X").ok();
}

#[test]
fn check_warns_when_period_copies_disagree() {
    let w = Work::new("period");
    let repeat = "<TU ALIGN=\"RIGHT\" AUNIT=\"PERIODTO2\" AUNITVALUE=\"20251231\" HEIGHT=\"23\" WIDTH=\"333\">2025년 12월 31일</TU>";
    let xml = w.contents().replacen("<TD HEIGHT=\"23\" WIDTH=\"249\">까지</TD>", repeat, 1);
    let v = w.variant("v.dsd", &xml);
    let r = dsd(["check", &s(&v), "--no-schema"]).ok();
    assert!(!r.to_string().contains("period-mismatch"));
    let run = dsd([
        "set-cell",
        &s(&v),
        "-o",
        &s(&w.path("o.dsd")),
        "--table",
        "1",
        "--row",
        "2",
        "--col",
        "1",
        "--text",
        "2026년 12월 31일",
        "--unit-value",
        "20261231",
    ]);
    run.ok();
    assert!(run.json()["warnings"].to_string().contains("period-mismatch"));
    let r = dsd(["check", &s(&w.path("o.dsd")), "--no-schema"]).ok();
    assert!(r.to_string().contains("PERIODTO/PERIODTO2 TU cells disagree: T1R2C1=20261231, T1R2C2=20251231"));
}

#[test]
fn currency_unit_value_is_a_code() {
    let w = Work::new("won");
    let unit = "<TU ALIGN=\"RIGHT\" AUNIT=\"WON\" AUNITVALUE=\"1\" HEIGHT=\"23\" WIDTH=\"249\">(단위 : 원)</TU>";
    let xml = w.contents().replacen("<TD HEIGHT=\"23\" WIDTH=\"249\">까지</TD>", unit, 1);
    let v = w.variant("v.dsd", &xml);
    assert!(!dsd(["check", &s(&v), "--no-schema"]).ok().to_string().contains("unit-value"));
    let run_with = |text: &str, value: &str, out: &str| {
        dsd([
            "set-cell",
            &s(&v),
            "-o",
            &s(&w.path(out)),
            "--table",
            "1",
            "--row",
            "2",
            "--col",
            "2",
            "--text",
            text,
            "--unit-value",
            value,
        ])
    };
    let run = run_with("(단위 : 천원)", "1000", "a.dsd");
    run.fails("edit_refused");
    assert!(run.error_text().contains("AUNIT=WON takes a code (1=원, 2=천원, 3=백만원), not \"1000\""));
    let run = run_with("(단위 : 천원)", "3", "b.dsd");
    assert!(run.error_text().contains("text shows 천원 (code 2) but AUNITVALUE is 3 (백만원)"));
    run_with("(단위 : 천원)", "2", "c.dsd").ok();
    assert!(contents_of(&w.path("c.dsd")).contains("AUNITVALUE=\"2\""));
    run_with("직원 (단위 : 천원)", "2", "d.dsd").ok();
    let stale = w.variant("stale.dsd", &xml.replace("(단위 : 원)", "(단위 : 백만원)"));
    let r = dsd(["check", &s(&stale), "--no-schema"]).ok();
    assert!(r.to_string().contains("T1R2C2: TU WON: text shows 백만원 (code 3) but AUNITVALUE is 1 (원)"));
}

#[test]
fn set_cell_fills_empty_and_self_closing_wrappers() {
    let w = Work::new("wrappers");
    let a = w.path("a.dsd");
    dsd(["copy-row", &s(&w.src()), "--table", "4", "--row", "3", "--clear", "-o", &s(&a)]).ok();
    set_cell(&a, &w.path("b.dsd"), "4", "4", "1", "김철수").ok();
    assert!(contents_of(&w.path("b.dsd")).contains("WIDTH=\"191\">김철수</TD>"));
    let bad = w.variant("bad.dsd", &w.contents().replace("<P>가나다테크 주식회사</P>", "<P USERMARK=\"B\"/>"));
    set_cell(&bad, &w.path("c.dsd"), "2", "1", "1", "NEW").ok();
    assert!(contents_of(&w.path("c.dsd")).contains("<P USERMARK=\"B\">NEW</P>"));
}

#[test]
fn negative_text_value_is_accepted() {
    let w = Work::new("negative");
    set_cell(&w.src(), &w.path("o.dsd"), "3", "4", "3", "-700,000").ok();
    assert!(contents_of(&w.path("o.dsd")).contains(">-700,000</TD>"));
}

#[test]
fn replace_guards_count_and_skips_markup() {
    let w = Work::new("replace");
    let o = s(&w.path("o.dsd"));
    let run = dsd(["replace", &s(&w.src()), "--old", "1,500,000", "--new", "1,600,000", "-o", &o]);
    run.fails("count_mismatch");
    assert!(run.error_text().contains("found 2 occurrence(s)"));
    dsd(["replace", &s(&w.src()), "--old", "1,500,000", "--new", "1,600,000", "--expect", "2", "-o", &o]).ok();
    dsd(["replace", &s(&w.src()), "--old", "NORMAL", "--new", "X", "-o", &s(&w.path("b.dsd"))]).fails("text_not_found");
    dsd(["replace", &s(&w.src()), "--old", "x", "--new", "y"]).fails("invalid_invocation");
}

#[test]
fn replace_ignores_entities_scaffolding_and_comments() {
    let w = Work::new("replace2");
    let o = s(&w.path("o.dsd"));
    dsd(["replace", &s(&w.src()), "--old", "&", "--new", "와", "--expect", "any", "-o", &o]).fails("text_not_found");
    let r = dsd(["replace", &s(&w.src()), "--old", "감사인", "--new", "X", "--dry-run"]).ok();
    assert_eq!(r["count"], 1);
    let v = w.variant("c.dsd", &w.contents().replace("<P>1. 일반사항</P>", "<P>1. 일반<!-- a > b 사항 -->사항</P>"));
    assert_eq!(dsd(["replace", &s(&v), "--old", "사항", "--new", "X", "--dry-run"]).ok()["count"], 1);
    dsd([
        "replace",
        &s(&w.src()),
        "--old",
        "하였습니다.\n둘째",
        "--new",
        "하였습니다. 둘째",
        "-o",
        &s(&w.path("b.dsd")),
    ])
    .ok();
    assert!(contents_of(&w.path("b.dsd")).contains("하였습니다. 둘째 줄입니다."));
}

#[test]
fn preexisting_errors_do_not_block_unrelated_edits() {
    let w = Work::new("preexisting");
    let v = w.variant("v.dsd", &w.contents().replace("COLSPAN=\"2\" HEIGHT=\"23\"", "COLSPAN=\"3\" HEIGHT=\"23\""));
    dsd(["copy-row", &s(&v), "--table", "3", "--row", "4", "-o", &s(&w.path("o.dsd"))]).ok();
}

#[test]
fn clear_empties_mixed_cells_and_unit_values() {
    let w = Work::new("clear");
    let r = dsd(["copy-row", &s(&w.src()), "--table", "2", "--row", "1", "--clear", "-o", &s(&w.path("o.dsd"))]).ok();
    assert!(r["changes"][0]["summary"].as_str().unwrap().contains("structure kept"));
    assert!(contents_of(&w.path("o.dsd")).contains("<P></P>\r\n<P></P>"));
}

#[test]
fn apply_validates_field_types() {
    let w = Work::new("apply-types");
    let edits = w.path("edits.json");
    for (bad, needle) in [
        (json!({"op": "set-cell", "table": 3, "row": 4, "col": 2, "text": null}), "missing field \"text\""),
        (json!({"op": "copy-row", "table": 3, "row": 4, "clear": "false"}), "true or false"),
        (json!({"op": "delete-row", "table": 3, "row": null}), "missing field \"row\""),
        (json!({"op": "set-cell", "table": 3, "row": 4, "col": 2, "txt": "1"}), "unknown field"),
        (json!({"op": "replace", "old": "a", "new": "b", "expect": "abc"}), "positive integer"),
        (json!({"op": "teleport"}), "unknown op"),
    ] {
        std::fs::write(&edits, json!([bad]).to_string()).unwrap();
        let run = dsd(["apply", &s(&w.src()), &s(&edits), "-o", &s(&w.path("o.dsd"))]);
        run.fails("invalid_argument");
        assert!(run.error_text().contains(needle), "{needle}: {}", run.stdout);
        assert!(run.error_text().starts_with("edit #1"));
    }
    std::fs::write(&edits, "{}").unwrap();
    dsd(["apply", &s(&w.src()), &s(&edits), "-o", &s(&w.path("o.dsd"))]).fails("invalid_argument");
}

#[test]
fn copy_then_delete_row_restores_bytes() {
    let w = Work::new("copydelete");
    let (a, b) = (w.path("a.dsd"), w.path("b.dsd"));
    dsd(["copy-row", &s(&w.src()), "--table", "3", "--row", "4", "--clear", "-o", &s(&a)]).ok();
    assert!(contents_of(&a).contains("<TD HEIGHT=\"23\" VALIGN=\"MIDDLE\" WIDTH=\"247\"></TD>"));
    dsd(["delete-row", &s(&a), "--table", "3", "--row", "5", "-o", &s(&b)]).ok();
    assert_eq!(contents_of(&b), w.contents());
}

#[test]
fn row_locks_and_rowspans_are_respected() {
    let w = Work::new("locks");
    let o = s(&w.path("o.dsd"));
    let run = dsd(["delete-row", &s(&w.src()), "--table", "3", "--row", "6", "-o", &o]);
    run.fails("edit_refused");
    assert!(run.error_text().contains("ADELETE=\"N\""));
    let run = dsd(["copy-row", &s(&w.src()), "--table", "4", "--row", "1", "-o", &o]);
    assert!(run.error_text().contains("ROWSPAN"));
    dsd(["delete-row", &s(&w.src()), "--table", "3", "--row", "6", "--force-locked", "-o", &o]).ok();
}

#[test]
fn apply_runs_edits_in_order() {
    let w = Work::new("apply");
    let edits = w.path("edits.json");
    std::fs::write(
        &edits,
        json!([
            {"op": "copy-row", "table": 4, "row": 3, "clear": true},
            {"op": "set-cell", "table": 4, "row": 4, "col": 1, "text": "김철수"},
            {"op": "replace", "old": "제 5 기", "new": "제 6 기", "table": 3},
        ])
        .to_string(),
    )
    .unwrap();
    let r = dsd(["apply", &s(&w.src()), &s(&edits), "-o", &s(&w.path("o.dsd"))]).ok();
    assert_eq!(r["changes"].as_array().unwrap().len(), 3);
    let diff = dsd(["diff", &s(&w.src()), &s(&w.path("o.dsd")), "--format", "text"]).stdout;
    assert!(diff.contains("\"제 5 기\" -> \"제 6 기\""));
    assert!(diff.contains("\"김철수\""));
}

fn diff_lines(a: &std::path::Path, b: &std::path::Path) -> Vec<String> {
    dsd(["diff", &s(a), &s(b), "--format", "text"])
        .stdout
        .lines()
        .filter(|l| l.starts_with("+ ") || l.starts_with("- ") || l.starts_with("~ "))
        .map(str::to_owned)
        .collect()
}

#[test]
fn diff_reports_inserted_row_at_its_coordinates() {
    let w = Work::new("diffrow");
    let xml = w.contents().replacen(
        "WIDTH=\"176\">500,000</TD>\r\n</TR>\r\n\r\n<TR ACOPY=\"N\"",
        "WIDTH=\"176\"></TD>\r\n</TR>\r\n\r\n<TR ACOPY=\"N\"",
        1,
    );
    let tail: String = (0..40).map(|i| format!("<P>꼬리 {i}</P>\r\n")).collect();
    let xml = xml.replacen("<PGBRK></PGBRK>\r\n</SECTION-2>", &format!("{tail}<PGBRK></PGBRK>\r\n</SECTION-2>"), 1);
    let base = w.variant("base.dsd", &xml);
    let edits = w.path("edits.json");
    std::fs::write(
        &edits,
        json!([
            {"op": "copy-row", "table": 3, "row": 5, "clear": true},
            {"op": "set-cell", "table": 3, "row": 6, "col": 1, "text": "   테스트"},
            {"op": "set-cell", "table": 3, "row": 6, "col": 2, "text": "(1,000)"},
        ])
        .to_string(),
    )
    .unwrap();
    let out = w.path("o.dsd");
    dsd(["apply", &s(&base), &s(&edits), "-o", &s(&out)]).ok();
    assert_eq!(diff_lines(&base, &out), ["+ T3R6C1 TD \"   테스트\"", "+ T3R6C2 TD \"(1,000)\"", "+ T3R6C3 TD \"\""]);
    let b = w.path("b.dsd");
    dsd(["delete-row", &s(&out), "--table", "3", "--row", "6", "-o", &s(&b)]).ok();
    let removed: Vec<String> = diff_lines(&out, &b).iter().map(|l| l.chars().take(9).collect()).collect();
    assert_eq!(removed, ["- T3R6C1 ", "- T3R6C2 ", "- T3R6C3 "]);
    let c = w.path("c.dsd");
    set_cell(&base, &c, "3", "3", "2", "999").ok();
    let text = dsd(["diff", &s(&base), &s(&c), "--format", "text"]).stdout;
    assert!(text.contains("~ T3R3C2 -> T3R3C2  \"1,500,000\" -> \"999\""));
    assert!(text.contains("1 changed block(s)"));
}

#[test]
fn diff_pairs_edited_row_next_to_inserted_row() {
    let w = Work::new("diffpair");
    let changes = |edits: Value| {
        let path = w.path("edits.json");
        std::fs::write(&path, edits.to_string()).unwrap();
        let out = w.path("adj.dsd");
        dsd(["apply", &s(&w.src()), &s(&path), "-o", &s(&out), "--force"]).ok();
        diff_lines(&w.src(), &out)
    };
    assert_eq!(
        changes(json!([
            {"op": "copy-row", "table": 3, "row": 4, "clear": true},
            {"op": "set-cell", "table": 3, "row": 5, "col": 1, "text": "   기타"},
            {"op": "set-cell", "table": 3, "row": 6, "col": 2, "text": "1,600,000"},
        ])),
        [
            "+ T3R5C1 TD \"   기타\"",
            "+ T3R5C2 TD \"\"",
            "+ T3R5C3 TD \"\"",
            "~ T3R5C2 -> T3R6C2  \"500,000\" -> \"1,600,000\"",
        ]
    );
    assert_eq!(
        changes(json!([
            {"op": "set-cell", "table": 3, "row": 5, "col": 3, "text": "X"},
            {"op": "copy-row", "table": 3, "row": 5, "clear": true},
            {"op": "set-cell", "table": 3, "row": 6, "col": 1, "text": "x"},
        ])),
        ["~ T3R5C3 -> T3R5C3  \"500,000\" -> \"X\"", "+ T3R6C1 TD \"x\"", "+ T3R6C2 TD \"\"", "+ T3R6C3 TD \"\""]
    );
}

#[test]
fn output_guards() {
    let w = Work::new("guards");
    set_cell(&w.src(), &w.src(), "3", "4", "2", "1").fails("output_exists");
    std::fs::write(w.path("o.dsd"), b"keep").unwrap();
    set_cell(&w.src(), &w.path("o.dsd"), "3", "4", "2", "1").fails("output_exists");
    assert_eq!(std::fs::read(w.path("o.dsd")).unwrap(), b"keep");
}

// ---------------------------------------------------------------------------
// extract and pack

#[test]
fn extract_pack_round_trip_is_byte_identical() {
    let w = Work::new("roundtrip");
    let work = w.path("work");
    let r = dsd(["extract", &s(&w.src()), &s(&work)]).ok();
    assert_eq!(r["members"].as_array().unwrap().len(), 3);
    let r = dsd(["pack", &s(&work), &s(&w.src()), "-o", &s(&w.path("o.dsd"))]).ok();
    assert_eq!(r["changedMembers"], json!([]));
    assert_eq!(std::fs::read(w.path("o.dsd")).unwrap(), std::fs::read(w.src()).unwrap());
}

#[test]
fn pack_refuses_structural_damage_line_endings_and_bom() {
    let w = Work::new("packguards");
    let work = w.path("work");
    dsd(["extract", &s(&w.src()), &s(&work)]).ok();
    let xml = std::fs::read_to_string(work.join("contents.xml")).unwrap();
    let pack = || dsd(["pack", &s(&work), &s(&w.src()), "-o", &s(&w.path("o.dsd"))]);

    std::fs::write(work.join("contents.xml"), xml.replacen("COLSPAN=\"2\"", "COLSPAN=\"3\"", 1)).unwrap();
    let e = pack().fails("structural_error");
    assert!(e["issues"].to_string().contains("table-grid"));
    assert!(!w.path("o.dsd").exists());

    std::fs::write(work.join("contents.xml"), xml.replace("\r\n", "\n")).unwrap();
    assert!(pack().fails("structural_error")["issues"].to_string().contains("line-endings"));

    std::fs::write(work.join("contents.xml"), format!("\u{feff}{xml}")).unwrap();
    assert!(pack().error_text().contains("BOM"));
}

#[test]
fn zip_comments_and_extra_fields_survive() {
    let w = Work::new("comments");
    let mut bytes = support::fixture_bytes();
    // Give the archive a comment: patch the end record's comment length and append it.
    let n = bytes.len();
    bytes[n - 2..].copy_from_slice(&12u16.to_le_bytes());
    bytes.extend_from_slice(b"archive note");
    std::fs::write(w.path("c.dsd"), &bytes).unwrap();
    set_cell(&w.path("c.dsd"), &w.path("o.dsd"), "3", "4", "2", "1").ok();
    assert_eq!(dsd::read_archive(&w.path("o.dsd")).unwrap().comment, b"archive note");
}

#[test]
fn read_extract_and_check_guards() {
    let w = Work::new("readguards");
    let run = dsd(["read", &s(&w.src()), "-o", &s(&w.src()), "--force"]);
    run.fails("output_exists");
    assert!(run.error_text().contains("source"));
    let run = dsd(["extract", &s(&w.src()), &s(&w.src())]);
    assert!(run.error_text().contains("not a directory"));
    let run = dsd(["check", &s(&w.src()), "--schema-dir", &s(&w.path("missing"))]);
    run.fails("file_not_found");
    assert!(run.error_text().contains("schema directory not found"));
}

// ---------------------------------------------------------------------------
// unverified content

fn note_variant(w: &Work) -> std::path::PathBuf {
    w.variant("note.dsd", &replace_once(&w.contents(), "<P>1. 일반사항</P>", "<NOTE><P>1. 일반사항</P></NOTE>"))
}

fn emphasis_in_table(w: &Work) -> std::path::PathBuf {
    let cell = "<TD HEIGHT=\"23\" VALIGN=\"MIDDLE\" WIDTH=\"247\">Ⅰ.유동자산</TD>";
    let xml = replace_once(&w.contents(), cell, &cell.replace(">Ⅰ.유동자산<", "><EMPHASIS>Ⅰ.유동자산</EMPHASIS><"));
    w.variant("emphasis.dsd", &xml)
}

#[test]
fn readers_flag_unverified_elements() {
    let w = Work::new("flag");
    let v = note_variant(&w);
    let r = dsd(["check", &s(&v), "--no-schema"]).ok();
    assert!(r.to_string().contains("unverified-element"));
    assert!(r.to_string().contains("<NOTE> x1"));
    let text = dsd(["read", &s(&v), "--format", "text"]).stdout;
    assert!(text.contains("# unverified: [\"NOTE\"]"));
    assert!(text.contains(" SECTION-2/NOTE\n"), "{text}");
    let f = dsd(["find", &s(&v), "일반사항"]).ok();
    assert_eq!(f["matches"][0]["unverified"], json!(["NOTE"]));
    assert!(dsd(["find", &s(&v), "일반사항", "--format", "text"]).stdout.contains("[unverified NOTE]"));
    assert!(dsd(["outline", &s(&v), "--all", "--format", "text"]).stdout.contains("<NOTE> unverified element"));
    let t = dsd(["table", &s(&emphasis_in_table(&w)), "3"]);
    assert_eq!(t.json()["warnings"][0]["code"], "unverified-table");
    assert!(
        dsd(["table", &s(&emphasis_in_table(&w)), "3", "--format", "text"])
            .stdout
            .contains("warning: contains unverified <EMPHASIS>")
    );
}

#[test]
fn edits_refuse_unverified_targets() {
    let w = Work::new("refuse");
    let v = note_variant(&w);
    let r = dsd(["replace", &s(&v), "--old", "일반사항", "--new", "X", "--dry-run"]).ok();
    assert_eq!(r["occurrences"][0]["unverified"], json!(["NOTE"]));
    let run = dsd(["replace", &s(&v), "--old", "일반사항", "--new", "X", "-o", &s(&w.path("o.dsd"))]);
    run.fails("unverified_content");
    assert!(run.error_text().contains("extract -> edit -> pack"));
    assert!(!w.path("o.dsd").exists());
    dsd(["replace", &s(&v), "--old", "첫 문단", "--new", "첫째 문단", "-o", &s(&w.path("o.dsd"))]).ok();

    let t = emphasis_in_table(&w);
    for args in [vec!["set-cell", "--col", "2", "--text", "1"], vec!["copy-row"], vec!["delete-row"]] {
        let mut full = vec![
            args[0].to_owned(),
            s(&t),
            "-o".into(),
            s(&w.path("t.dsd")),
            "--table".into(),
            "3".into(),
            "--row".into(),
            "4".into(),
        ];
        full.extend(args[1..].iter().map(|a| (*a).to_owned()));
        let run = dsd(&full);
        run.fails("unverified_content");
        assert!(run.error_text().contains("T3 contains <EMPHASIS>"));
    }
    set_cell(&t, &w.path("t.dsd"), "4", "3", "1", "X").ok();
}

#[test]
fn edits_refuse_tables_inside_unverified_containers() {
    let w = Work::new("container");
    let tail = "</TABLE>\r\n\r\n<IMAGE>";
    let xml = replace_once(&w.contents(), "<P>1. 일반사항</P>", "<P>1. 일반사항</P><NOTE>");
    let v = w.variant("nt.dsd", &replace_once(&xml, tail, "</TABLE></NOTE>\r\n\r\n<IMAGE>"));
    let o = s(&w.path("n.dsd"));
    for args in [
        vec!["set-cell", &s(&v), "-o", &o, "--table", "4", "--row", "3", "--col", "1", "--text", "X"],
        vec!["copy-row", &s(&v), "-o", &o, "--table", "4", "--row", "3"],
        vec!["delete-row", &s(&v), "-o", &o, "--table", "4", "--row", "3"],
        vec!["replace", &s(&v), "-o", &o, "--old", "홍길동", "--new", "X"],
        vec!["replace", &s(&v), "-o", &o, "--old", "홍길동", "--new", "X", "--table", "4"],
    ] {
        dsd(&args).fails("unverified_content");
    }
    let edits = w.path("edits.json");
    std::fs::write(&edits, json!([{"op": "replace", "old": "홍길동", "new": "X", "table": 4}]).to_string()).unwrap();
    dsd(["apply", &s(&v), &s(&edits), "-o", &o]).fails("unverified_content");
    assert!(!w.path("n.dsd").exists());
    let r = dsd(["replace", &s(&v), "--old", "홍길동", "--new", "X", "--table", "4", "--dry-run"]).ok();
    assert_eq!(r["occurrences"][0]["unverified"], json!(["NOTE"]));
    assert!(dsd(["table", &s(&v), "4", "--format", "text"]).stdout.contains("warning: lies inside unverified <NOTE>"));
}

#[test]
fn replace_refuses_unverified_before_counting() {
    let w = Work::new("before-count");
    let run = dsd([
        "replace",
        &s(&note_variant(&w)),
        "--old",
        "일반사항",
        "--new",
        "X",
        "--expect",
        "3",
        "-o",
        &s(&w.path("o.dsd")),
    ]);
    run.fails("unverified_content");
    assert!(!run.error_text().contains("expected 3"));
}

fn renamed_section(w: &Work, level: u8) -> std::path::PathBuf {
    let xml = w
        .contents()
        .replace("<SECTION-2 ", &format!("<SECTION-{level} "))
        .replace("</SECTION-2>", &format!("</SECTION-{level}>"));
    w.variant(&format!("section{level}.dsd"), &xml)
}

#[test]
fn unverified_section_keeps_its_outline_title() {
    let w = Work::new("section8");
    let out = dsd(["outline", &s(&renamed_section(&w, 8)), "--format", "text"]).stdout;
    assert!(out.contains("SECTION-8 재무상태표 [unverified element]"));
}

#[test]
fn schema_equivalent_elements_are_editable() {
    let w = Work::new("section4");
    let deep = renamed_section(&w, 4);
    assert_eq!(dsd(["check", &s(&deep), "--no-schema"]).ok()["issues"], json!([]));
    assert!(dsd(["outline", &s(&deep), "--format", "text"]).stdout.contains("SECTION-4 재무상태표\n"));
    let r = dsd(["read", &s(&deep)]).ok();
    assert!(text_rows(&r).contains(&("SECTION-4/TITLE".into(), "재무상태표".into())));
    assert_eq!(r["unverified"], json!([]));
    set_cell(&deep, &w.path("a.dsd"), "3", "4", "2", "1,100,000").ok();
    dsd(["replace", &s(&deep), "--old", "1. 일반사항", "--new", "1. 일반 사항", "-o", &s(&w.path("b.dsd"))]).ok();
    let footed = w.variant("tfoot.dsd", &w.contents().replacen("</TBODY>", "</TBODY>\r\n<TFOOT></TFOOT>", 1));
    assert_eq!(dsd(["check", &s(&footed), "--no-schema"]).ok()["issues"], json!([]));
}

#[test]
fn unverified_elements_inside_scaffolding_are_ignored() {
    let w = Work::new("scaffold");
    let comment = "<COMMENT>◆click◆ 예시: 정정 안내 문단 삽입</COMMENT>";
    let v = w.variant(
        "s.dsd",
        &replace_once(&w.contents(), comment, &comment.replace("◆click◆", "<EMPHASIS>◆click◆</EMPHASIS>")),
    );
    assert_eq!(dsd(["check", &s(&v), "--no-schema"]).ok()["issues"], json!([]));
    assert_eq!(dsd(["read", &s(&v)]).ok()["unverified"], json!([]));
}

#[test]
fn unverified_schema_blocks_edits() {
    let w = Work::new("schema9");
    let v = w.variant(
        "s.dsd",
        &w.contents().replace("noNamespaceSchemaLocation=\"dart4.xsd\"", "noNamespaceSchemaLocation=\"dart9.xsd\""),
    );
    assert!(dsd(["check", &s(&v), "--no-schema"]).ok().to_string().contains("unverified-schema"));
    assert_eq!(dsd(["read", &s(&v)]).ok()["unverified"], json!(["schema dart9.xsd"]));
    let run = set_cell(&v, &w.path("o.dsd"), "3", "4", "2", "1");
    run.fails("unverified_content");
    assert!(run.error_text().contains("schema dart9.xsd"));
    let run = dsd(["replace", &s(&v), "--old", "첫 문단", "--new", "X", "-o", &s(&w.path("o.dsd"))]);
    assert!(run.error_text().contains("schema dart9.xsd"));
}

#[test]
fn raw_route_handles_unverified_elements() {
    let w = Work::new("rawroute");
    let v = note_variant(&w);
    let work = w.path("work");
    dsd(["extract", &s(&v), &s(&work)]).ok();
    let xml = std::fs::read_to_string(work.join("contents.xml")).unwrap();
    std::fs::write(work.join("contents.xml"), xml.replace("<P>1. 일반사항</P>", "<P>1. 일반 사항</P>")).unwrap();
    let r = dsd(["pack", &s(&work), &s(&v), "-o", &s(&w.path("o.dsd"))]).ok();
    assert_eq!(r["changedMembers"], json!(["contents.xml"]));
    assert!(contents_of(&w.path("o.dsd")).contains("<NOTE><P>1. 일반 사항</P></NOTE>"));
    assert!(dsd(["diff", &s(&v), &s(&w.path("o.dsd")), "--format", "text"]).stdout.contains("일반 사항"));
}

// ---------------------------------------------------------------------------
// safety

#[test]
fn rejects_doctype() {
    let w = Work::new("doctype");
    let bad = w.zip(
        "bad.dsd",
        &[
            (b"contents.xml".to_vec(), b"<!DOCTYPE x [<!ENTITY a \"b\">]><DOCUMENT/>".to_vec()),
            (b"meta.xml".to_vec(), b"<METAINFO/>".to_vec()),
        ],
    );
    let e = dsd(["check", &s(&bad), "--no-schema"]).fails("check_failed");
    let _ = e;
    assert!(dsd(["check", &s(&bad), "--no-schema"]).stdout.contains("DOCTYPE"));
}

#[test]
fn rejects_path_traversal() {
    let w = Work::new("traversal");
    let bad = w.zip("bad.dsd", &[(b"../contents.xml".to_vec(), b"<DOCUMENT/>".to_vec())]);
    let run = dsd(["inspect", &s(&bad)]);
    run.fails("invalid_archive");
    assert!(run.error_text().contains("unsafe member path"));
}

#[test]
fn reports_missing_image() {
    let w = Work::new("noimage");
    let bad = w.zip(
        "bad.dsd",
        &[
            (b"contents.xml".to_vec(), support::crlf(&support::source("contents.xml"))),
            (b"meta.xml".to_vec(), support::crlf(&support::source("meta.xml"))),
        ],
    );
    let run = dsd(["check", &s(&bad), "--no-schema"]);
    let v = run.json();
    assert_eq!(v["error"]["code"], "check_failed");
    assert!(v["result"]["issues"].to_string().contains("image-missing"));
}

#[test]
fn rejects_corrupt_archives() {
    let w = Work::new("corrupt");
    std::fs::write(w.path("x.dsd"), b"not a zip").unwrap();
    dsd(["inspect", &s(&w.path("x.dsd"))]).fails("invalid_archive");
    let mut bytes = support::fixture_bytes();
    // Past the 30-byte local header and the 12-byte name: the compressed data.
    bytes[30 + 12 + 5] ^= 0xff;
    std::fs::write(w.path("y.dsd"), &bytes).unwrap();
    dsd(["inspect", &s(&w.path("y.dsd"))]).fails("invalid_archive");
}

// ---------------------------------------------------------------------------
// resource bounds

#[test]
fn huge_spans_stay_bounded() {
    let w = Work::new("hugespan");
    let xml = replace_once(
        &w.contents(),
        "<TD HEIGHT=\"23\" VALIGN=\"MIDDLE\" WIDTH=\"191\">홍길동</TD>",
        "<TD COLSPAN=\"100000000000\">홍길동</TD>",
    );
    let v = w.variant("span.dsd", &xml);
    let run = dsd(["check", &s(&v), "--no-schema"]);
    assert!(run.json()["result"]["issues"].to_string().contains("table-grid"));
    let no_colgroup = w.variant(
        "nocols.dsd",
        &replace_once(
            &w.contents(),
            "<P>1. 일반사항</P>",
            "<TABLE><TBODY><TR><TD COLSPAN=\"100000000000000\">x</TD></TR></TBODY></TABLE>",
        ),
    );
    let run = dsd(["check", &s(&no_colgroup), "--no-schema"]);
    assert!(run.json()["result"]["issues"].to_string().contains("grid is limited"));
}

#[test]
fn deep_nesting_is_refused() {
    let w = Work::new("deep");
    let deep = format!("<P>{}x{}</P>", "<SPAN>".repeat(5000), "</SPAN>".repeat(5000));
    let v = w.variant("deep.dsd", &replace_once(&w.contents(), "<P>1. 일반사항</P>", &deep));
    dsd(["read", &s(&v)]).fails("invalid_xml");
    assert!(dsd(["check", &s(&v), "--no-schema"]).stdout.contains("nested too deeply"));
}

#[test]
fn non_utf8_declarations_are_refused() {
    let w = Work::new("encoding");
    let v = w.variant("enc.dsd", &w.contents().replacen("encoding=\"utf-8\"", "encoding=\"EUC-KR\"", 1));
    let run = dsd(["read", &s(&v)]);
    run.fails("invalid_xml");
    assert!(run.error_text().contains("must be UTF-8"));
}

#[test]
fn find_stops_at_the_limit_like_the_helper() {
    let w = Work::new("findlimit");
    let r = dsd(["find", &s(&w.src()), "000", "--limit", "2"]).ok();
    assert_eq!(r["matches"].as_array().unwrap().len(), 2);
    assert_eq!(r["limited"], true);
}

#[test]
fn oversized_declared_grids_are_reported_not_built() {
    let w = Work::new("biggrid");
    let cols = "<COL WIDTH=\"1\"></COL>".repeat(2000);
    let rows = "<TR></TR>".repeat(2000);
    let table = format!("<TABLE><COLGROUP>{cols}</COLGROUP><TBODY>{rows}</TBODY></TABLE>");
    let v = w.variant("big.dsd", &replace_once(&w.contents(), "<P>1. 일반사항</P>", &table));
    let run = dsd(["check", &s(&v), "--no-schema"]);
    assert!(run.json()["result"]["issues"].to_string().contains("grid limit"));
    let issues = run.json()["result"]["issues"].as_array().unwrap().len();
    assert!(issues < 50, "{issues} issues");
    dsd(["table", &s(&v), "4"]).fails("invalid_argument");
    // Many malformed rows report a bounded problem list.
    let ragged = format!(
        "<TABLE><COLGROUP>{}</COLGROUP><TBODY>{}</TBODY></TABLE>",
        "<COL WIDTH=\"1\"></COL>".repeat(3),
        "<TR></TR>".repeat(500)
    );
    let v = w.variant("ragged.dsd", &replace_once(&w.contents(), "<P>1. 일반사항</P>", &ragged));
    let run = dsd(["check", &s(&v), "--no-schema"]);
    let text = run.json()["result"]["issues"].to_string();
    assert!(text.contains("more grid problem(s)"), "{text}");
}

#[test]
fn check_against_keeps_issues_when_the_edit_does_not_parse() {
    let w = Work::new("against-broken");
    let broken = w.variant("broken.dsd", &w.contents().replacen("</DOCUMENT>", "", 1));
    let run = dsd(["check", &s(&broken), "--against", &s(&w.src()), "--no-schema"]);
    let v = run.json();
    assert_eq!(v["error"]["code"], "check_failed");
    assert!(v["result"]["issues"].to_string().contains("contents-xml"));
}

#[test]
fn malformed_xml_declarations_are_refused() {
    let w = Work::new("decl");
    let v = w.variant("decl.dsd", &w.contents().replacen("<?xml version=\"1.0\" encoding=\"utf-8\"?>", "<?xml?>", 1));
    assert!(dsd(["check", &s(&v), "--no-schema"]).stdout.contains("contents-xml"));
}
