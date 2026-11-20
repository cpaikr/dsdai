//! The checks file: the agent's overlay on the proposals.
//!
//! The file holds only what the agent decided: checks it states (a check
//! whose `id` matches a proposal replaces it, any other is added) and
//! dismissals of proposals, each with its reason. The CLI computes every
//! value; the file never carries one.
//!
//! A file written against one draft may be applied to a later one, where
//! rows and tables have moved. Every entry therefore names its target by
//! `T/R/C` and by the row label, column label, and section it saw there; an
//! entry applies only when they are still at its `T/R/C`. When they are at
//! exactly one other cell (in its section, else in the tables around it),
//! the entry is reported as moved with that cell, else as stale; neither is
//! applied. Only the target is verified: two tables with the same labels in
//! one section can trade places unseen, and a row inserted inside a term's
//! range changes that term's sum (which then shows as a failure).

use std::collections::{HashMap, HashSet};

use serde::{Deserialize, Serialize};

use super::{CellRef, Check, Context, Origin, Proposals, Tolerance, col_label, row_label};
use crate::doc::Doc;
use crate::text::compact;

/// How many tables before or after its own a moved entry's labels are looked
/// for (a table inserted or removed nearby renumbers the rest).
const NEARBY_TABLES: usize = 3;

#[derive(Debug, Clone, Default, PartialEq, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct ChecksFile {
    /// Checks the agent states: replacements (by a proposal's id) and additions.
    #[serde(default)]
    pub checks: Vec<Check>,
    /// Proposals the agent rejects.
    #[serde(default)]
    pub dismiss: Vec<Dismissal>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct Dismissal {
    pub id: String,
    /// The proposal's target, with its labels, as the agent saw it.
    pub target: CellRef,
    pub reason: String,
}

/// A proposal a checks file dismissed.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct Dismissed {
    pub id: String,
    pub target: CellRef,
    pub reason: String,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "kebab-case")]
pub enum Entry {
    Check,
    Dismissal,
}

/// A checks-file entry that did not apply because its labels are no longer
/// at its `T/R/C`.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct Unapplied {
    pub entry: Entry,
    pub id: String,
    /// The target and labels the entry states.
    pub stated: CellRef,
    /// What the document has at that `T/R/C` now.
    pub there: CellRef,
    /// Where the stated labels are now, for a moved entry.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub found: Option<CellRef>,
    /// Why a stale entry did not apply.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub reason: Option<String>,
}

#[derive(Debug, Default)]
pub(super) struct Applied {
    pub dismissed: Vec<Dismissed>,
    pub moved: Vec<Unapplied>,
    pub stale: Vec<Unapplied>,
}

impl ChecksFile {
    /// Read a checks file, refusing anything the CLI could not apply as
    /// written: unknown fields, a check that is not the agent's or lacks
    /// its reason, a dismissal without one, or an id used twice.
    pub fn parse(bytes: &[u8]) -> Result<Self, String> {
        let text = bytes.strip_prefix(b"\xef\xbb\xbf").unwrap_or(bytes);
        let value: serde_json::Value = serde_json::from_slice(text).map_err(|e| format!("not a checks file: {e}"))?;
        // serde would also read a struct from an array; a checks file is an object.
        if !value.is_object() {
            return Err("not a checks file: expected an object with \"checks\" and \"dismiss\"".to_owned());
        }
        let file: Self = serde_json::from_value(value).map_err(|e| format!("not a checks file: {e}"))?;
        let mut ids = HashSet::new();
        for (i, c) in file.checks.iter().enumerate() {
            let at = format!("checks[{i}] ({})", c.id);
            if c.id.trim().is_empty() {
                return Err(format!("checks[{i}]: a check needs an id"));
            }
            if c.origin != Origin::Agent {
                return Err(format!("{at}: a checks file states agent checks; set \"origin\": \"agent\""));
            }
            if c.rule.is_some() {
                return Err(format!("{at}: \"rule\" names a CLI proposer and is not set by the agent"));
            }
            if c.note.as_deref().is_none_or(|n| n.trim().is_empty()) {
                return Err(format!("{at}: a \"note\" saying why the relation holds is required"));
            }
            if c.terms.is_empty() {
                return Err(format!("{at}: a check needs at least one term"));
            }
            if let Some(Tolerance::Absolute(x) | Tolerance::PerTerm(x)) = c.tolerance
                && !(x.is_finite() && x >= 0.0)
            {
                return Err(format!("{at}: a tolerance is a finite amount of 0 or more"));
            }
            numbered(&c.target).map_err(|e| format!("{at}: {e}"))?;
            if !ids.insert(c.id.as_str()) {
                return Err(format!("{at}: the id appears more than once"));
            }
        }
        for (i, d) in file.dismiss.iter().enumerate() {
            let at = format!("dismiss[{i}] ({})", d.id);
            if d.id.trim().is_empty() {
                return Err(format!("dismiss[{i}]: a dismissal needs the id it dismisses"));
            }
            if d.reason.trim().is_empty() {
                return Err(format!("{at}: a dismissal needs a reason"));
            }
            numbered(&d.target).map_err(|e| format!("{at}: {e}"))?;
            if !ids.insert(d.id.as_str()) {
                return Err(format!("{at}: the id appears more than once"));
            }
        }
        Ok(file)
    }
}

fn numbered(c: &CellRef) -> Result<(), String> {
    if c.table == 0 || c.row == 0 || c.col == 0 {
        return Err("tables, rows, and columns are numbered from 1".to_owned());
    }
    Ok(())
}

/// Where an entry's labels are.
enum Place {
    Here,
    Moved(CellRef),
    Stale(String),
}

/// A label as compared across drafts: without spacing and one leading list
/// marker (`Ⅰ.`, `1.`, `1)`, `(1)`, `가.`, `(가)`, `①`), so renumbering does
/// not matter but every other character does: `2024년` is not `2025년`,
/// `USD` is not `CNY`, and `차입금(원화)` is not `차입금(외화)`.
fn label_key(label: &str) -> String {
    let s = compact(label);
    let rest = strip_marker(&s);
    if rest.is_empty() { s } else { rest.to_owned() }
}

fn strip_marker(s: &str) -> &str {
    let roman = |c: char| ('\u{2160}'..='\u{217f}').contains(&c);
    let ordinal = |c: char| "가나다라마바사아자차카타파하".contains(c);
    let Some(first) = s.chars().next() else { return s };
    if roman(first) {
        let rest = s.trim_start_matches(roman);
        return rest.strip_prefix('.').unwrap_or(rest);
    }
    if let Some(dot) = s.find('.')
        && dot > 0
        && s[..dot].chars().all(|c| matches!(c, 'I' | 'V' | 'X' | 'L'))
    {
        return &s[dot + 1..];
    }
    let digits = s.len() - s.trim_start_matches(|c: char| c.is_ascii_digit()).len();
    if (1..=2).contains(&digits) {
        let rest = &s[digits..];
        if let Some(r) = rest.strip_prefix(')') {
            return r;
        }
        if let Some(r) = rest.strip_prefix('.')
            && !r.starts_with(|c: char| c.is_ascii_digit())
        {
            return r;
        }
    }
    let width = first.len_utf8();
    if ordinal(first) && s[width..].starts_with('.') {
        return &s[width + 1..];
    }
    if ('\u{2460}'..='\u{2473}').contains(&first) {
        return &s[width..];
    }
    if first == '('
        && let Some(close) = s.find(')')
    {
        let inner = &s[1..close];
        let numbered = (1..=2).contains(&inner.len()) && inner.bytes().all(|b| b.is_ascii_digit());
        let lettered = inner.chars().count() == 1 && inner.chars().all(ordinal);
        if numbered || lettered {
            return &s[close + 1..];
        }
    }
    s
}

/// Whether a stated label is the document's: equal keys. An unstated label
/// matches anything; a stated one never matches a missing one.
fn same(stated: Option<&str>, now: Option<&str>) -> bool {
    match (stated, now) {
        (None, _) => true,
        (Some(a), Some(b)) => label_key(a) == label_key(b),
        (Some(_), None) => false,
    }
}

/// The same section, when both the entry and the document name one.
fn same_section(stated: Option<&str>, now: Option<&str>) -> bool {
    match (stated, now) {
        (Some(a), Some(b)) => label_key(a) == label_key(b),
        _ => true,
    }
}

/// A table's row label keys and column label keys.
type TableKeys = (Vec<Option<String>>, Vec<Option<String>>);

/// Each table's label keys, read once across all entries.
struct Keys<'a> {
    doc: &'a Doc,
    tables: HashMap<usize, TableKeys>,
}

impl<'a> Keys<'a> {
    fn new(doc: &'a Doc) -> Self {
        Self { doc, tables: HashMap::new() }
    }

    fn of(&mut self, t: usize) -> &TableKeys {
        let doc = self.doc;
        self.tables.entry(t).or_insert_with(|| {
            let table = &doc.tables[t - 1];
            let rows = (1..=table.rows.len()).map(|r| row_label(doc, table, r).map(|l| label_key(&l))).collect();
            let cols = (1..=table.ncols()).map(|c| col_label(doc, table, c).map(|l| label_key(&l))).collect();
            (rows, cols)
        })
    }
}

fn place(doc: &Doc, ctx: &Context, keys: &mut Keys, stated: &CellRef) -> Place {
    let (t, r, c) = stated.position();
    let origin = |t: usize, r: usize, c: usize| {
        let table = doc.table(t).ok().filter(|table| !table.oversized)?;
        doc.cell_at(table, r, c).ok().map(|cell| (t, cell.row, cell.col))
    };
    let now = CellRef::at(doc, ctx, t, r, c);
    let fits = same(stated.row_label.as_deref(), now.row_label.as_deref())
        && same(stated.col_label.as_deref(), now.col_label.as_deref())
        && same_section(stated.section.as_deref(), now.section.as_deref());
    if origin(t, r, c).is_some() && fits {
        return Place::Here;
    }
    if stated.row_label.is_none() && stated.col_label.is_none() {
        return Place::Stale("its cell does not hold what it states, and it states no labels to look for".to_owned());
    }
    // Look in the entry's section when it names one the document has, else
    // in the tables around its own. An unstated label keeps its row or column.
    let in_section = stated
        .section
        .as_deref()
        .filter(|s| (1..=doc.tables.len()).any(|tt| ctx.section(tt).is_some_and(|x| same(Some(s), Some(x)))));
    let scope: Vec<usize> = match in_section {
        Some(s) => {
            (1..=doc.tables.len()).filter(|&tt| ctx.section(tt).is_some_and(|x| same(Some(s), Some(x)))).collect()
        }
        None => (t.saturating_sub(NEARBY_TABLES).max(1)..=(t + NEARBY_TABLES).min(doc.tables.len())).collect(),
    };
    let (row_key, col_key) = (stated.row_label.as_deref().map(label_key), stated.col_label.as_deref().map(label_key));
    let mut found: Vec<(usize, usize, usize)> = Vec::new();
    for tt in scope {
        if doc.tables[tt - 1].oversized {
            continue;
        }
        let (rows, cols) = keys.of(tt);
        let pick = |labels: &[Option<String>], key: &Option<String>, own: usize| -> Vec<usize> {
            match key {
                Some(k) => (1..=labels.len()).filter(|&i| labels[i - 1].as_ref() == Some(k)).collect(),
                None => vec![own],
            }
        };
        let (rows, cols) = (pick(rows, &row_key, r), pick(cols, &col_key, c));
        for &rr in &rows {
            for &cc in &cols {
                if let Some(cell) = origin(tt, rr, cc)
                    && cell != (t, r, c)
                    && !found.contains(&cell)
                {
                    found.push(cell);
                }
            }
        }
    }
    let scope = if in_section.is_some() { "in its section" } else { "in the tables around it" };
    match found.as_slice() {
        [] => Place::Stale(format!("its labels are not at its cell or elsewhere {scope}")),
        [(tt, rr, cc)] => Place::Moved(CellRef::at(doc, ctx, *tt, *rr, *cc)),
        many => Place::Stale(format!("its labels are at {} cells {scope}; restate it for one of them", many.len())),
    }
}

/// Apply a checks file to the proposals: dismissals remove proposals,
/// checks replace or join them, and entries whose labels moved are reported
/// instead of applied.
pub(super) fn apply(doc: &Doc, ctx: &Context, file: &ChecksFile, proposals: &mut Proposals) -> Applied {
    let mut applied = Applied::default();
    let mut keys = Keys::new(doc);
    let unapplied = |entry: Entry, id: &str, stated: &CellRef, found: Option<CellRef>, reason: Option<String>| {
        let (t, r, c) = stated.position();
        Unapplied {
            entry,
            id: id.to_owned(),
            stated: stated.clone(),
            there: CellRef::at(doc, ctx, t, r, c),
            found,
            reason,
        }
    };
    for d in &file.dismiss {
        let (t, r, c) = d.target.position();
        match place(doc, ctx, &mut keys, &d.target) {
            Place::Here => {
                if matches!(take(proposals, &d.id, d.target.position()), Taken::Nothing) {
                    let why = "no proposal has this id at this cell any more".to_owned();
                    applied.stale.push(unapplied(Entry::Dismissal, &d.id, &d.target, None, Some(why)));
                } else {
                    let target = CellRef::at(doc, ctx, t, r, c);
                    applied.dismissed.push(Dismissed { id: d.id.clone(), target, reason: d.reason.clone() });
                }
            }
            Place::Moved(at) => applied.moved.push(unapplied(Entry::Dismissal, &d.id, &d.target, Some(at), None)),
            Place::Stale(why) => applied.stale.push(unapplied(Entry::Dismissal, &d.id, &d.target, None, Some(why))),
        }
    }
    let mut stated = Vec::new();
    for c in &file.checks {
        match place(doc, ctx, &mut keys, &c.target) {
            Place::Here => stated.push(c),
            Place::Moved(at) => applied.moved.push(unapplied(Entry::Check, &c.id, &c.target, Some(at), None)),
            Place::Stale(why) => applied.stale.push(unapplied(Entry::Check, &c.id, &c.target, None, Some(why))),
        }
    }
    // A cell the agent now checks is covered.
    let targets: HashSet<(usize, usize, usize)> = stated.iter().map(|c| c.target.position()).collect();
    proposals.uncovered.retain(|&(_, t, r, c)| !targets.contains(&(t, r, c)));
    for c in stated {
        // A check replaces the proposal with its id at its target, which
        // keeps its place among the checks; any other check, or one replacing
        // an unresolved total or a candidate, follows them.
        match take(proposals, &c.id, c.target.position()) {
            Taken::Check(i) => proposals.checks.insert(i, c.clone()),
            Taken::Other | Taken::Nothing => proposals.checks.push(c.clone()),
        }
    }
    applied
}

/// Where a removed proposal was.
enum Taken {
    /// Check `i`.
    Check(usize),
    /// An unresolved total or a candidate.
    Other,
    Nothing,
}

/// Remove the proposal with `id` at `position` from the checks, unresolved
/// totals, or candidates. A `#2` id names the second proposal at one cell in
/// the order the proposers emit them.
fn take(proposals: &mut Proposals, id: &str, position: (usize, usize, usize)) -> Taken {
    let matches = |c: &Check| c.id == id && c.target.position() == position;
    if let Some(i) = proposals.checks.iter().position(matches) {
        proposals.checks.remove(i);
        return Taken::Check(i);
    }
    for list in [&mut proposals.unresolved, &mut proposals.candidates] {
        if let Some(i) = list.iter().position(matches) {
            list.remove(i);
            return Taken::Other;
        }
    }
    Taken::Nothing
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::foot::testing::{doc, table};
    use crate::foot::{Report, Status, foot, foot_with};

    /// A note whose 합계 is 5 off in the prior period, so its proposed check
    /// fails there; 기타 holds a figure the agent relates to the prior one.
    fn note(extra_row: bool) -> Doc {
        let mut rows: Vec<&[&str]> = vec![&["예금", "100", "80"], &["현금", "20", "10"]];
        if extra_row {
            rows.insert(0, &["신규", "0", "0"]);
        }
        rows.extend([&["기타", "3", "3"] as &[&str], &["합 계", "123", "98"]]);
        doc(&format!("<P>(단위: 원)</P>{}", table(&["구 분", "당 기", "전 기"], &rows)))
    }

    fn parse(json: &str) -> ChecksFile {
        ChecksFile::parse(json.as_bytes()).unwrap()
    }

    fn statuses(r: &Report) -> Vec<(String, Status)> {
        r.checks.iter().map(|c| (format!("{} {:?}", c.id, c.origin), c.status)).collect()
    }

    const FILE: &str = r#"{
        "checks": [
            {"id": "v:T1.R5.C3", "kind": "vertical", "origin": "agent",
             "target": {"table": 1, "row": 5, "col": 3, "rowLabel": "합 계", "colLabel": "전 기"},
             "terms": [{"sign": 1, "table": 1, "rows": [2, 4], "col": 3}],
             "tolerance": {"absolute": 5}, "note": "the prior period was rounded in the filed draft"},
            {"id": "a:other", "kind": "cross", "origin": "agent",
             "target": {"table": 1, "row": 4, "col": 2, "rowLabel": "기타"},
             "terms": [{"sign": 1, "table": 1, "row": 4, "col": 3}], "note": "기타 is unchanged"}
        ],
        "dismiss": [
            {"id": "v:T1.R5.C2", "target": {"table": 1, "row": 5, "col": 2, "rowLabel": "합 계", "colLabel": "당 기"},
             "reason": "checked by hand"}
        ]
    }"#;

    #[test]
    fn a_checks_file_replaces_adds_and_dismisses() {
        let before = foot(&note(false));
        assert_eq!(
            statuses(&before),
            [("v:T1.R5.C2 Auto".to_owned(), Status::Pass), ("v:T1.R5.C3 Auto".to_owned(), Status::Fail)]
        );
        let r = foot_with(&note(false), &parse(FILE));
        assert_eq!(
            statuses(&r),
            [("v:T1.R5.C3 Agent".to_owned(), Status::Pass), ("a:other Agent".to_owned(), Status::Pass)],
            "the failing proposal is replaced with a stated tolerance; the dismissed one is gone"
        );
        assert!((r.checks[0].allowed - 5.0).abs() < f64::EPSILON);
        assert_eq!(r.checks[0].note.as_deref(), Some("the prior period was rounded in the filed draft"));
        let dismissed: Vec<(&str, &str)> = r.dismissed.iter().map(|d| (d.id.as_str(), d.reason.as_str())).collect();
        assert_eq!(dismissed, [("v:T1.R5.C2", "checked by hand")]);
        assert_eq!((r.counts.dismissed, r.counts.moved, r.counts.stale), (1, 0, 0));
    }

    #[test]
    fn a_file_reapplied_to_a_later_draft_reports_moved_and_stale_entries() {
        // A row inserted at the top moves every labelled cell down by one.
        let r = foot_with(&note(true), &parse(FILE));
        let moved: Vec<(&str, Entry, (usize, usize, usize))> =
            r.moved.iter().map(|m| (m.id.as_str(), m.entry, m.found.as_ref().unwrap().position())).collect();
        assert_eq!(
            moved,
            [
                ("v:T1.R5.C2", Entry::Dismissal, (1, 6, 2)),
                ("v:T1.R5.C3", Entry::Check, (1, 6, 3)),
                ("a:other", Entry::Check, (1, 5, 2)),
            ]
        );
        assert_eq!(r.moved[1].there.row_label.as_deref(), Some("기타"), "what the stated cell holds now");
        assert!(r.checks.iter().all(|c| c.origin == crate::foot::Origin::Auto), "nothing moved is applied");
        // A label that is gone everywhere is stale.
        let gone = parse(&FILE.replace("\"rowLabel\": \"기타\"", "\"rowLabel\": \"없는 항목\""));
        let r = foot_with(&note(false), &gone);
        let stale: Vec<&str> = r.stale.iter().map(|s| s.id.as_str()).collect();
        assert_eq!(stale, ["a:other"]);
        assert!(r.stale[0].reason.as_deref().unwrap().contains("not at its cell"));
    }

    #[test]
    fn labels_compare_without_spacing_or_numbering_but_otherwise_exactly() {
        let same_key = |a: &str, b: &str| label_key(a) == label_key(b);
        assert!(same_key("Ⅰ. 유 동 자 산", "Ⅱ.유동자산"));
        assert!(same_key("4. 현금및현금성자산", "5. 현금및현금성자산"), "notes renumber between drafts");
        assert!(same_key("(1) 당기", "(2) 당기"));
        assert!(same_key("가. 기타", "나.기타"));
        assert!(!same_key("2024년", "2025년"));
        assert!(!same_key("2024 / 금액", "2023 / 금액"));
        assert!(!same_key("USD", "CNY"));
        assert!(!same_key("차입금(원화)", "차입금(외화)"));
        assert!(!same_key("(주)갑", "(주)을"));
        assert_eq!(label_key("2024.01.01 (전기초)"), "2024.01.01(전기초)");
        assert_eq!(label_key("1.5"), "1.5");
    }

    #[test]
    fn a_column_inserted_before_a_year_column_moves_the_entry() {
        let years = |cols: &[&str], rows: &[&[&str]]| doc(&table(cols, rows));
        let before = years(&["구 분", "2024년", "2023년"], &[&["갑", "1", "2"], &["합 계", "1", "2"]]);
        let after =
            years(&["구 분", "2025년", "2024년", "2023년"], &[&["갑", "9", "1", "2"], &["합 계", "9", "1", "2"]]);
        let file = parse(
            r#"{"checks": [{"id": "a:1", "kind": "vertical", "origin": "agent",
                "target": {"table": 1, "row": 3, "col": 2, "rowLabel": "합 계", "colLabel": "2024년"},
                "terms": [{"sign": 1, "table": 1, "row": 2, "col": 2}], "note": "n"}]}"#,
        );
        assert_eq!(statuses(&foot_with(&before, &file)).last().unwrap().0, "a:1 Agent");
        let r = foot_with(&after, &file);
        assert!(r.checks.iter().all(|c| c.id != "a:1"), "2025년 is not 2024년");
        assert_eq!(r.moved[0].found.as_ref().unwrap().position(), (1, 3, 3));
    }

    /// Notes 2 and 3 of a 주석 section, each with a same-shaped table.
    fn notes(first: &str, second: &str) -> Doc {
        let t = |a: &str, b: &str| table(&["구 분", "당 기"], &[&["갑", a], &["합 계", b]]);
        doc(&format!(
            "<SECTION-1><TITLE>주 석</TITLE><P>1. 일반사항</P><P>{first}</P>{}<P>{second}</P>{}</SECTION-1>",
            t("10", "10"),
            t("20", "20")
        ))
    }

    #[test]
    fn a_table_inserted_in_another_note_does_not_capture_the_entry() {
        // The entry was written for 현금 (note 2, T1); a later draft inserts a
        // note 2 with a same-shaped table, so 현금 is note 3 and T2.
        let file = parse(
            r#"{"dismiss": [{"id": "v:T1.R3.C2", "reason": "r",
                "target": {"table": 1, "row": 3, "col": 2, "rowLabel": "합 계", "colLabel": "당 기", "section": "2. 현금"}}]}"#,
        );
        let r = foot_with(&notes("2. 매출채권", "3. 현금"), &file);
        assert!(r.dismissed.is_empty(), "the same labels in another note are not the entry's cell");
        assert_eq!(r.moved[0].found.as_ref().unwrap().position(), (2, 3, 2));
        assert_eq!(r.moved[0].there.section.as_deref(), Some("2. 매출채권"));
        // Unmoved, the renumbered note still matches.
        let file = parse(
            r#"{"dismiss": [{"id": "v:T2.R3.C2", "reason": "r",
            "target": {"table": 2, "row": 3, "col": 2, "rowLabel": "합 계", "section": "2. 현금"}}]}"#,
        );
        assert_eq!(foot_with(&notes("2. 매출채권", "3. 현금"), &file).dismissed.len(), 1);
    }

    #[test]
    fn labels_at_several_cells_make_an_entry_stale() {
        let d = doc(&table(&["구 분", "당 기"], &[&["갑", "1"], &["소 계", "1"], &["을", "2"], &["소 계", "2"]]));
        let file = parse(
            r#"{"dismiss": [{"id": "x", "target": {"table": 1, "row": 9, "col": 2, "rowLabel": "소 계"}, "reason": "r"}]}"#,
        );
        let r = foot_with(&d, &file);
        assert!(r.moved.is_empty());
        assert!(r.stale[0].reason.as_deref().unwrap().contains("at 2 cells"));
    }

    #[test]
    fn a_replacement_keeps_its_place_and_must_name_its_proposal_s_cell() {
        let check = |id: &str, row: usize| {
            format!(
                r#"{{"checks": [{{"id": "{id}", "kind": "vertical", "origin": "agent",
                    "target": {{"table": 1, "row": {row}, "col": 2}},
                    "terms": [{{"sign": 1, "table": 1, "rows": [2, 4], "col": 2}}], "note": "n"}}]}}"#
            )
        };
        let r = foot_with(&note(false), &parse(&check("v:T1.R5.C2", 5)));
        assert_eq!(
            statuses(&r),
            [("v:T1.R5.C2 Agent".to_owned(), Status::Pass), ("v:T1.R5.C3 Auto".to_owned(), Status::Fail)],
            "the replacement stands where its proposal stood"
        );
        // The proposal's id at another cell replaces nothing: it adds a check.
        let r = foot_with(&note(false), &parse(&check("v:T1.R5.C2", 4)));
        assert_eq!(r.checks.len(), 3);
        assert_eq!(r.checks[0].origin, crate::foot::Origin::Auto);
    }

    #[test]
    fn a_check_replaces_an_unresolved_total() {
        let d = doc(&table(&["구 분", "당기"], &[&["합 계", "100"], &["갑", "10"], &["을", "20"], &["합 계", "50"]]));
        assert_eq!(foot(&d).unresolved[0].id, "v:T1.R5.C2");
        let file = parse(
            r#"{"checks": [{"id": "v:T1.R5.C2", "kind": "vertical", "origin": "agent",
                "target": {"table": 1, "row": 5, "col": 2, "rowLabel": "합 계"},
                "terms": [{"sign": 1, "table": 1, "rows": [3, 4], "col": 2}], "tolerance": {"absolute": 20},
                "note": "rounded"}]}"#,
        );
        let r = foot_with(&d, &file);
        assert!(r.unresolved.is_empty());
        assert_eq!(statuses(&r), [("v:T1.R5.C2 Agent".to_owned(), Status::Pass)]);
    }

    #[test]
    fn a_dismissal_of_a_proposal_that_is_gone_is_stale() {
        let file = parse(
            r#"{"dismiss": [{"id": "v:T1.R4.C2", "target": {"table": 1, "row": 4, "col": 2, "rowLabel": "기타"}, "reason": "x"}]}"#,
        );
        let r = foot_with(&note(false), &file);
        assert!(r.dismissed.is_empty());
        assert_eq!(r.stale[0].reason.as_deref(), Some("no proposal has this id at this cell any more"));
    }

    #[test]
    fn a_check_on_an_uncovered_total_covers_it() {
        let d = doc(&table(&["구 분", "당기"], &[&["합 계", "30"], &["갑", "10"], &["을", "20"]]));
        assert_eq!(foot(&d).uncovered.len(), 1);
        let file = parse(
            r#"{"checks": [{"id": "a:1", "kind": "vertical", "origin": "agent",
                "target": {"table": 1, "row": 2, "col": 2, "rowLabel": "합 계"},
                "terms": [{"sign": 1, "table": 1, "rows": [3, 4], "col": 2}], "note": "the total heads its rows"}]}"#,
        );
        let r = foot_with(&d, &file);
        assert!(r.uncovered.is_empty());
        assert_eq!(statuses(&r), [("a:1 Agent".to_owned(), Status::Pass)]);
    }

    #[test]
    fn invalid_checks_files_are_refused_with_the_entry_named() {
        let check = |patch: &str| {
            format!(
                r#"{{"checks": [{{"id": "a:1", "kind": "vertical", "origin": "agent",
                    "target": {{"table": 1, "row": 2, "col": 2}},
                    "terms": [{{"sign": 1, "table": 1, "row": 3, "col": 2}}], "note": "n"{patch}}}]}}"#
            )
        };
        assert!(ChecksFile::parse(check("").as_bytes()).is_ok());
        assert!(ChecksFile::parse(format!("\u{feff}{}", check("")).as_bytes()).is_ok(), "a BOM is accepted");
        let refused = |json: &str| ChecksFile::parse(json.as_bytes()).unwrap_err();
        assert!(refused("[]").starts_with("not a checks file"));
        assert!(refused(r#"{"checks": [], "extra": 1}"#).contains("unknown field"));
        assert!(
            refused(&check("").replace("\"agent\"", "\"auto\""))
                .contains("checks[0] (a:1): a checks file states agent checks")
        );
        assert!(refused(&check("").replace("\"note\": \"n\"", "\"note\": \" \"")).contains("\"note\""));
        assert!(refused(&check(", \"rule\": \"label-total\"")).contains("\"rule\""));
        assert!(refused(&check(", \"tolerance\": {\"absolute\": -1}")).contains("tolerance"));
        assert!(refused(&check("").replace("\"row\": 2", "\"row\": 0")).contains("numbered from 1"));
        let twice = r#"{"checks": [], "dismiss": [
            {"id": "x", "target": {"table": 1, "row": 1, "col": 1}, "reason": "r"},
            {"id": "x", "target": {"table": 1, "row": 1, "col": 1}, "reason": "r"}]}"#;
        assert!(refused(twice).contains("dismiss[1] (x): the id appears more than once"));
        assert!(
            refused(r#"{"dismiss": [{"id": "x", "target": {"table": 1, "row": 1, "col": 1}, "reason": ""}]}"#)
                .contains("reason")
        );
    }
}
