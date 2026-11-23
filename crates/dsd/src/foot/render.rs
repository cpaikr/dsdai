//! The footing workbook: the xlsx export plus a live formula per check.
//!
//! Every check becomes `= target − (Σ sign × SUM(term))`, its difference, so
//! a correction typed into the workbook updates it. Where it sits follows the
//! convention of hand-made footing workbooks:
//!
//! - a check down its table (a column total, a subtotal over the detail
//!   column, a movement) sits in a check row below the table, in the
//!   target's column;
//! - a check across a row (a total column, the balance identity across
//!   columns) sits right of the table, in the target's row;
//! - every check is listed on the `검증` sheet, which holds the formula of a
//!   check between tables and refers to the in-table cell of the others; the
//!   index sheet counts its result column by formula.
//!
//! A difference of 0 shows as ✓; one beyond its allowance is red. A check
//! that needs judgment gets no formula; the `검증` sheet gives its reason.

use std::collections::{BTreeMap, HashSet};
use std::fmt::Write as _;

use super::{CheckResult, Origin, Report, Status, Unit, scale, table_units};
use crate::doc::Doc;
use crate::error::Result;
use crate::sheets::{
    Cell, FIRST_CONTENT_ROW, Flag, INDEX_SHEET, Placement, Role, Sheet, SheetKind, Value, Workbook, cell_ref,
    index_sheet, workbook_with,
};
use crate::text::collapse;
use crate::xlsx::{quoted, write};

/// The checks sheet's name.
pub const CHECKS_SHEET: &str = "검증";
/// A difference: as computed, and ✓ for 0.
const DIFFERENCE_FORMAT: &str = "General;-General;\"✓\"";
const PASSED: &str = "통과";
const DIFFERENT: &str = "차이";
const JUDGMENT: &str = "판단 필요";

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Spot {
    /// Check row `row` below the table (0-based), in grid column `col` (1-based).
    Below { row: usize, col: usize },
    /// Check column `col` right of the table (0-based), in grid row `row` (1-based).
    Beside { row: usize, col: usize },
    /// On the checks sheet only.
    Sheet,
}

/// One check row below a table, or one check column beside it: the target
/// row or column it serves, and the columns or rows its checks take.
struct Slot {
    key: usize,
    taken: Vec<usize>,
}

/// Where each check's formula sits, and how many check rows each table needs.
/// Check rows follow their target rows; check columns their target columns.
fn plan(report: &Report) -> (Vec<Spot>, BTreeMap<usize, usize>) {
    let mut spots = vec![Spot::Sheet; report.checks.len()];
    let mut below: BTreeMap<usize, Vec<Slot>> = BTreeMap::new();
    let mut beside: BTreeMap<usize, Vec<Slot>> = BTreeMap::new();
    let mut order: Vec<usize> = (0..report.checks.len()).collect();
    order.sort_by_key(|&i| {
        let t = &report.checks[i].target;
        (t.table, t.row, t.col, i)
    });
    for &i in &order {
        let c = &report.checks[i];
        let (t, r, col) = c.target.position();
        if c.status == Status::NeedsJudgment || c.terms.iter().any(|term| term.table != t) {
            continue;
        }
        let across = c.terms.iter().all(|term| term.rows == (r, r));
        let (slots, key, use_) =
            if across { (beside.entry(t).or_default(), col, r) } else { (below.entry(t).or_default(), r, col) };
        let s = slots.iter().position(|s| s.key == key && !s.taken.contains(&use_)).unwrap_or_else(|| {
            slots.push(Slot { key, taken: Vec::new() });
            slots.len() - 1
        });
        slots[s].taken.push(use_);
        spots[i] = if across { Spot::Beside { row: r, col: s } } else { Spot::Below { row: s, col } };
    }
    // Slots open in check order; number them by the row or column they serve.
    let ranks = |slots: &Vec<Slot>| {
        let mut by_key: Vec<usize> = (0..slots.len()).collect();
        by_key.sort_by_key(|&s| (slots[s].key, s));
        let mut rank = vec![0; slots.len()];
        for (new, &old) in by_key.iter().enumerate() {
            rank[old] = new;
        }
        rank
    };
    let (below_rank, beside_rank): (BTreeMap<usize, Vec<usize>>, BTreeMap<usize, Vec<usize>>) =
        (below.iter().map(|(&t, s)| (t, ranks(s))).collect(), beside.iter().map(|(&t, s)| (t, ranks(s))).collect());
    for (i, spot) in spots.iter_mut().enumerate() {
        let t = report.checks[i].target.table;
        *spot = match *spot {
            Spot::Below { row, col } => Spot::Below { row: below_rank[&t][row], col },
            Spot::Beside { row, col } => Spot::Beside { row, col: beside_rank[&t][col] },
            Spot::Sheet => Spot::Sheet,
        };
    }
    (spots, below.into_iter().map(|(t, slots)| (t, slots.len())).collect())
}

/// The footing workbook model and its xlsx bytes.
pub fn render(doc: &Doc, report: &Report) -> Result<(Workbook, Vec<u8>)> {
    let (spots, rows) = plan(report);
    // One blank row under a table, then its check rows.
    let mut model = workbook_with(doc, &|t| rows.get(&t).map_or(0, |&n| 1 + n as u32))?;
    let placement: BTreeMap<usize, Placement> = model.placements.iter().map(|p| (p.table, *p)).collect();
    let units = table_units(doc);
    let mut at: Vec<Option<(usize, u32, u16)>> = vec![None; report.checks.len()];
    for (i, (check, spot)) in report.checks.iter().zip(&spots).enumerate() {
        let Some(p) = placement.get(&check.target.table) else { continue };
        let (row, col) = match *spot {
            Spot::Below { row, col } => (p.row + p.rows + 1 + row as u32, p.col + col as u16 - 1),
            Spot::Beside { row, col } => (p.row + row as u32 - 1, p.col + p.cols + 1 + col as u16),
            Spot::Sheet => continue,
        };
        let Some(f) = formula(doc, &model, &placement, &units, check, p.sheet) else { continue };
        let sheet = &mut model.sheets[p.sheet];
        sheet.cells.push(difference(row, col, f, check));
        sheet.flags.push(Flag { row, col, allowed: check.allowed });
        at[i] = Some((p.sheet, row, col));
    }
    label_slots(&mut model, report, &spots, &placement);
    let (sheet, results) = checks_sheet(doc, &model, &placement, &units, report, &at);
    model.sheets.push(sheet);
    model.sheets[0] = live_index(&model, report, &results);
    for sheet in &mut model.sheets {
        let used = sheet.cells.iter().map(|c| c.row + 1).max().unwrap_or(0);
        sheet.rows = sheet.rows.max(used);
    }
    model.counts.sheets = model.sheets.len();
    let bytes = write(&model)?;
    Ok((model, bytes))
}

/// `=ROUND(target-(SUM(a)-SUM(b)+1000*SUM(c)),k)`, with references local to
/// sheet `home` and qualified elsewhere; None when a table is not in the
/// workbook. A spanning cell's figure is at its merge's first cell, so every
/// position is read through the cell that owns it, as the CLI reads it.
/// Rounding to the places the figures carry (`k`) removes binary noise and
/// keeps every difference, as the evaluator does.
fn formula(
    doc: &Doc,
    model: &Workbook,
    placement: &BTreeMap<usize, Placement>,
    units: &[Option<Unit>],
    check: &CheckResult,
    home: usize,
) -> Option<String> {
    let at = |p: &Placement, (r, c): (usize, usize)| cell_ref(p.row + r as u32 - 1, p.col + c as u16 - 1, false);
    let qualify = |p: &Placement, refs: String| {
        if p.sheet == home { refs } else { format!("{}!{refs}", quoted(&model.sheets[p.sheet].name)) }
    };
    let origin = |table: usize, r: usize, c: usize| -> Option<(usize, usize)> {
        let cell = doc.cell_at(doc.table(table).ok()?, r, c).ok()?;
        Some((cell.row, cell.col))
    };
    // A range, when the cells owning its positions all start inside it;
    // else the owning cells one by one (a span entering from outside).
    let reference = |table: usize, rows: (usize, usize), cols: (usize, usize)| -> Option<String> {
        let p = placement.get(&table)?;
        let mut owners: Vec<(usize, usize)> = Vec::new();
        let mut seen: HashSet<(usize, usize)> = HashSet::new();
        for r in rows.0..=rows.1 {
            for c in cols.0..=cols.1 {
                let o = origin(table, r, c)?;
                if seen.insert(o) {
                    owners.push(o);
                }
            }
        }
        let inside = owners.iter().all(|&(r, c)| (rows.0..=rows.1).contains(&r) && (cols.0..=cols.1).contains(&c));
        let refs = if inside && rows.0 == rows.1 && cols.0 == cols.1 {
            at(p, (rows.0, cols.0))
        } else if inside {
            format!("{}:{}", at(p, (rows.0, cols.0)), at(p, (rows.1, cols.1)))
        } else {
            return Some(owners.iter().map(|&o| qualify(p, at(p, o))).collect::<Vec<_>>().join(","));
        };
        Some(qualify(p, refs))
    };
    let t = &check.target;
    let target = origin(t.table, t.row, t.col)?;
    let mut f = format!("=ROUND({}-(", reference(t.table, (target.0, target.0), (target.1, target.1))?);
    for (k, term) in check.terms.iter().enumerate() {
        let range = reference(term.table, term.rows, term.cols)?;
        f.push_str(match (k, term.sign < 0) {
            (_, true) => "-",
            (0, false) => "",
            (_, false) => "+",
        });
        let factor = scale(units, term.table, t.table).unwrap_or(1.0);
        if (factor - 1.0).abs() > f64::EPSILON {
            let _ = write!(f, "{factor}*");
        }
        let _ = write!(f, "SUM({range})");
    }
    let _ = write!(f, "),{})", places(check));
    Some(f)
}

/// The places a difference formula rounds to: those the figures carry (as
/// the evaluator rounds), or more while binary noise stays below them, so a
/// value typed into the workbook with more decimals still shows (14
/// significant digits of the checked amounts).
fn places(check: &CheckResult) -> u32 {
    // log10 of a finite magnitude of at least 1: a whole number below 310.
    #[allow(clippy::cast_possible_truncation, clippy::cast_sign_loss)]
    let digits = check.magnitude.max(1.0).log10().ceil() as u32;
    check.places.max(14u32.saturating_sub(digits))
}

fn cell(row: u32, col: u16, value: Value, role: Role) -> Cell {
    Cell { row, col, value: Some(value), role, border: false, wrap: false }
}

fn text(row: u32, col: u16, value: impl Into<String>, role: Role) -> Cell {
    cell(row, col, Value::Text(value.into()), role)
}

fn difference(row: u32, col: u16, formula: String, check: &CheckResult) -> Cell {
    // The evaluator's difference is already rounded as the formula rounds it.
    let result = check.difference.map_or_else(String::new, |d| format!("{d}"));
    cell(row, col, Value::Formula { formula, result, format: Some(DIFFERENCE_FORMAT.to_owned()) }, Role::Plain)
}

/// Name each check row below a table after its target row, and each check
/// column beside it after its target column, in grey, with who stated the
/// checks in it.
fn label_slots(model: &mut Workbook, report: &Report, spots: &[Spot], placement: &BTreeMap<usize, Placement>) {
    struct Label<'a> {
        text: String,
        taken: Vec<usize>,
        origins: Vec<&'a str>,
    }
    let mut rows: BTreeMap<(usize, usize), Label> = BTreeMap::new();
    let mut cols: BTreeMap<(usize, usize), Label> = BTreeMap::new();
    for (check, spot) in report.checks.iter().zip(spots) {
        let t = &check.target;
        let named = |l: &Option<String>| l.as_deref().map_or_else(String::new, |l| format!(" {}", collapse(l)));
        let (map, key, label, taken) = match *spot {
            Spot::Below { row, col } => {
                (&mut rows, (t.table, row), format!("검증 R{}{}", t.row, named(&t.row_label)), col)
            }
            Spot::Beside { row, col } => {
                (&mut cols, (t.table, col), format!("차이 C{}{}", t.col, named(&t.col_label)), row)
            }
            Spot::Sheet => continue,
        };
        let entry = map.entry(key).or_insert_with(|| Label { text: label, taken: Vec::new(), origins: Vec::new() });
        entry.taken.push(taken);
        entry.origins.push(match check.origin {
            Origin::Auto => "auto",
            Origin::Agent => "agent",
        });
    }
    let shown = |l: &mut Label| {
        l.origins.sort_unstable();
        l.origins.dedup();
        format!("{} ({})", l.text, l.origins.join("/"))
    };
    // A row's label goes in the table's first column, a column's in its first
    // row, unless a check is there.
    for ((t, slot), mut label) in rows {
        let Some(p) = placement.get(&t) else { continue };
        if !label.taken.contains(&1) {
            let row = p.row + p.rows + 1 + slot as u32;
            model.sheets[p.sheet].cells.push(text(row, p.col, shown(&mut label), Role::Marker));
        }
    }
    for ((t, slot), mut label) in cols {
        let Some(p) = placement.get(&t) else { continue };
        if !label.taken.contains(&1) {
            let col = p.col + p.cols + 1 + slot as u16;
            model.sheets[p.sheet].cells.push(text(p.row, col, shown(&mut label), Role::Marker));
        }
    }
}

/// Column headers of the checks sheet, by 0-based column.
const COLUMNS: [&str; 11] = ["시트", "위치", "ID", "종류", "출처", "규칙", "대상", "차이", "허용", "결과", "비고"];
const DIFF_COL: u16 = 7;
const ALLOWED_COL: u16 = 8;
const RESULT_COL: u16 = 9;
const NOTE_COL: u16 = 10;

fn kebab(value: &impl serde::Serialize) -> String {
    serde_json::to_value(value).ok().and_then(|v| v.as_str().map(str::to_owned)).unwrap_or_default()
}

/// One row per check: where it is, what it states, its difference (the
/// in-table cell, or a formula of its own), and a result formula the index
/// sheet counts. Returns the sheet and each check's result as written.
fn checks_sheet(
    doc: &Doc,
    model: &Workbook,
    placement: &BTreeMap<usize, Placement>,
    units: &[Option<Unit>],
    report: &Report,
    at: &[Option<(usize, u32, u16)>],
) -> (Sheet, Vec<&'static str>) {
    let taken: Vec<&str> = model.sheets.iter().map(|s| s.name.as_str()).collect();
    // Of sheets.len() + 1 candidates at least one is free.
    let name = (1..=model.sheets.len() + 1)
        .map(|n| if n == 1 { CHECKS_SHEET.to_owned() } else { format!("{CHECKS_SHEET} {n}") })
        .find(|n| !taken.contains(&n.as_str()))
        .unwrap_or_else(|| CHECKS_SHEET.to_owned());
    let mut cells = vec![
        text(0, 0, CHECKS_SHEET, Role::Title),
        cell(1, 0, Value::Link { sheet: 0, text: INDEX_SHEET.to_owned() }, Role::Plain),
    ];
    for (col, header) in COLUMNS.iter().enumerate() {
        cells.push(text(FIRST_CONTENT_ROW - 1, col as u16, *header, Role::Header));
    }
    let mut flags = Vec::new();
    let mut results = Vec::new();
    let home = model.sheets.len();
    for (i, check) in report.checks.iter().enumerate() {
        let row = FIRST_CONTENT_ROW + i as u32;
        let t = &check.target;
        let sheet = placement.get(&t.table).map(|p| model.sheets[p.sheet].name.clone()).unwrap_or_default();
        let label =
            [&t.row_label, &t.col_label].into_iter().flatten().map(|l| collapse(l)).collect::<Vec<_>>().join(" / ");
        cells.push(text(row, 0, sheet, Role::Plain));
        cells.push(text(row, 1, format!("T{} R{} C{}", t.table, t.row, t.col), Role::Plain));
        cells.push(text(row, 2, check.id.clone(), Role::Plain));
        cells.push(text(row, 3, kebab(&check.kind), Role::Plain));
        cells.push(text(row, 4, kebab(&check.origin), Role::Plain));
        cells.push(text(row, 5, check.rule.as_ref().map(kebab).unwrap_or_default(), Role::Plain));
        cells.push(text(row, 6, label, Role::Plain));
        let diff = match at[i] {
            Some((s, r, c)) => Some(format!("={}!{}", quoted(&model.sheets[s].name), cell_ref(r, c, false))),
            None if check.status == Status::NeedsJudgment => None,
            None => formula(doc, model, placement, units, check, home),
        };
        let mut note = check.reason.clone().or_else(|| check.note.clone());
        if let Some(f) = diff {
            cells.push(difference(row, DIFF_COL, f, check));
            flags.push(Flag { row, col: DIFF_COL, allowed: check.allowed });
            cells.push(cell(
                row,
                ALLOWED_COL,
                Value::Number { value: check.allowed, format: "General".to_owned() },
                Role::Plain,
            ));
            let (d, a) = (cell_ref(row, DIFF_COL, false), cell_ref(row, ALLOWED_COL, false));
            let result = if check.status == Status::Pass { PASSED } else { DIFFERENT };
            let formula = format!("=IF(ABS({d})<={a},\"{PASSED}\",\"{DIFFERENT}\")");
            cells.push(cell(
                row,
                RESULT_COL,
                Value::Formula { formula, result: result.to_owned(), format: None },
                Role::Plain,
            ));
            results.push(result);
        } else {
            if check.status != Status::NeedsJudgment {
                note = Some("a table of this check is not in the workbook".to_owned());
            }
            cells.push(text(row, RESULT_COL, JUDGMENT, Role::Plain));
            results.push(JUDGMENT);
        }
        if let Some(n) = note {
            cells.push(text(row, NOTE_COL, n, Role::Plain));
        }
    }
    let sheet = Sheet {
        name,
        kind: SheetKind::Checks,
        title: CHECKS_SHEET.to_owned(),
        cells,
        merges: Vec::new(),
        widths: BTreeMap::from([(0, 140), (1, 96), (2, 150), (6, 240), (NOTE_COL, 320)]),
        frozen_rows: FIRST_CONTENT_ROW,
        tables: Vec::new(),
        rows: FIRST_CONTENT_ROW + report.checks.len() as u32,
        flags,
    };
    (sheet, results)
}

/// The index sheet with each sheet's checks counted by formula over the
/// checks sheet, so the counts follow corrections, and a total row.
fn live_index(model: &Workbook, report: &Report, results: &[&str]) -> Sheet {
    let mut index = index_sheet(&model.sheets);
    let checks = quoted(&model.sheets.last().map(|s| s.name.clone()).unwrap_or_default());
    // The listed rows only (from row 4): the sheet's title and headers are not checks.
    let listed_end = FIRST_CONTENT_ROW + (report.checks.len() as u32).max(1);
    let column = |c: &str| format!("{checks}!${c}${}:${c}${listed_end}", FIRST_CONTENT_ROW + 1);
    let (sheet_col, result_col) = (column("A"), column("J"));
    let counted = ["검증", PASSED, DIFFERENT, JUDGMENT];
    for (k, h) in counted.iter().enumerate() {
        index.cells.push(text(FIRST_CONTENT_ROW - 1, 3 + k as u16, *h, Role::Header));
        index.widths.insert(3 + k as u16, 72);
    }
    let sheet_of: Vec<Option<usize>> = report
        .checks
        .iter()
        .map(|c| model.placements.iter().find(|p| p.table == c.target.table).map(|p| p.sheet))
        .collect();
    let count = |sheet: Option<usize>, result: Option<&str>| {
        (0..results.len())
            .filter(|&i| sheet.is_none_or(|s| sheet_of[i] == Some(s)) && result.is_none_or(|r| results[i] == r))
            .count()
            .to_string()
    };
    let mut last = FIRST_CONTENT_ROW;
    for (i, sheet) in model.sheets.iter().enumerate().skip(1) {
        let row = FIRST_CONTENT_ROW + i as u32 - 1;
        last = last.max(row);
        if sheet.kind == SheetKind::Checks {
            continue;
        }
        let name = sheet.name.replace('"', "\"\"");
        // `=` makes the name a literal: `<참고>` would otherwise compare.
        let mut by = format!("COUNTIFS({sheet_col},\"={name}\"");
        let total = (format!("={by})"), count(Some(i), None));
        by.push(',');
        index.cells.push(cell(row, 3, Value::Formula { formula: total.0, result: total.1, format: None }, Role::Plain));
        for (k, r) in [PASSED, DIFFERENT, JUDGMENT].into_iter().enumerate() {
            let formula = format!("={by}{result_col},\"{r}\")");
            let value = Value::Formula { formula, result: count(Some(i), Some(r)), format: None };
            index.cells.push(cell(row, 4 + k as u16, value, Role::Plain));
        }
    }
    let total = last + 2;
    index.cells.push(text(total, 0, "합계", Role::Bold));
    let all = format!("=COUNTA({})", column("C"));
    index.cells.push(cell(
        total,
        3,
        Value::Formula { formula: all, result: count(None, None), format: None },
        Role::Bold,
    ));
    for (k, r) in [PASSED, DIFFERENT, JUDGMENT].into_iter().enumerate() {
        let formula = format!("=COUNTIFS({result_col},\"{r}\")");
        let value = Value::Formula { formula, result: count(None, Some(r)), format: None };
        index.cells.push(cell(total, 4 + k as u16, value, Role::Bold));
    }
    index
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::foot::foot;
    use crate::foot::testing::{doc, table};

    /// Formulas by A1 cell, without the leading `=`.
    fn formulas(sheet: &Sheet) -> Vec<(String, String)> {
        let mut out: Vec<(u32, u16, String)> = sheet
            .cells
            .iter()
            .filter_map(|c| match &c.value {
                Some(Value::Formula { formula, .. }) => {
                    Some((c.row, c.col, formula.trim_start_matches('=').to_owned()))
                }
                _ => None,
            })
            .collect();
        out.sort();
        out.into_iter().map(|(r, c, f)| (cell_ref(r, c, false), f)).collect()
    }

    fn at<'a>(sheet: &'a Sheet, a1: &str) -> Option<&'a Value> {
        sheet.cells.iter().find(|c| cell_ref(c.row, c.col, false) == a1).and_then(|c| c.value.as_ref())
    }

    fn shown(sheet: &Sheet, a1: &str) -> String {
        match at(sheet, a1) {
            Some(Value::Text(t)) => t.clone(),
            Some(Value::Formula { result, .. }) => result.clone(),
            other => format!("{other:?}"),
        }
    }

    fn pairs(list: &[(&str, &str)]) -> Vec<(String, String)> {
        list.iter().map(|&(a, b)| (a.to_owned(), b.to_owned())).collect()
    }

    #[test]
    fn checks_down_a_table_sit_below_it_and_across_a_row_beside_it() {
        // 합 계 down each column, and 합계 across each row.
        let d = doc(&format!(
            "<P>(단위: 원)</P>{}<P>다음 문단</P>",
            table(
                &["구 분", "갑", "을", "합계"],
                &[&["가", "1", "2", "3"], &["나", "4", "5", "9"], &["합 계", "5", "7", "13"]],
            )
        ));
        let (wb, bytes) = render(&d, &foot(&d)).unwrap();
        assert!(!bytes.is_empty());
        let sheet = &wb.sheets[1];
        assert_eq!(wb.placements[0].range(), "A6:D9");
        // The marker is E6; check columns start at F, check rows after one blank row.
        assert_eq!(
            formulas(sheet),
            pairs(&[
                ("F7", "ROUND(D7-(SUM(B7:C7)),13)"),
                ("F8", "ROUND(D8-(SUM(B8:C8)),12)"),
                ("F9", "ROUND(D9-(SUM(B9:C9)),12)"),
                ("B11", "ROUND(B9-(SUM(B7:B8)),13)"),
                ("C11", "ROUND(C9-(SUM(C7:C8)),12)"),
                ("D11", "ROUND(D9-(SUM(D7:D8)),12)"),
            ])
        );
        assert_eq!(shown(sheet, "A11"), "검증 R4 합 계 (auto)");
        assert_eq!(shown(sheet, "F6"), "차이 C4 합계 (auto)");
        assert_eq!(shown(sheet, "F7"), "0", "the difference the CLI computed, shown as a check mark");
        // The next paragraph moves below the check row, after a blank row.
        assert_eq!(shown(sheet, "A13"), "다음 문단");
        assert_eq!(sheet.flags.len(), 6);
    }

    #[test]
    fn the_checks_sheet_lists_every_check_and_the_index_counts_them() {
        // 합 계 holds in 당기, so the prior period's difference is a finding.
        let d = doc(&table(&["구 분", "당기", "전기"], &[&["가", "1", "1"], &["나", "2", "2"], &["합 계", "3", "4"]]));
        let report = foot(&d);
        assert_eq!((report.counts.pass, report.counts.fail), (1, 1));
        let (wb, _) = render(&d, &report).unwrap();
        let checks = wb.sheets.last().unwrap();
        assert_eq!((checks.name.as_str(), checks.kind), ("검증", SheetKind::Checks));
        assert_eq!(
            formulas(checks),
            pairs(&[
                ("H4", "'본문'!B9"),
                ("J4", "IF(ABS(H4)<=I4,\"통과\",\"차이\")"),
                ("H5", "'본문'!C9"),
                ("J5", "IF(ABS(H5)<=I5,\"통과\",\"차이\")"),
            ])
        );
        assert_eq!((shown(checks, "J4"), shown(checks, "J5")), ("통과".to_owned(), "차이".to_owned()));
        let index = &wb.sheets[0];
        let counts = formulas(index);
        assert_eq!(counts[0], ("D4".to_owned(), "COUNTIFS('검증'!$A$4:$A$5,\"=본문\")".to_owned()));
        assert_eq!(
            counts[2],
            ("F4".to_owned(), "COUNTIFS('검증'!$A$4:$A$5,\"=본문\",'검증'!$J$4:$J$5,\"차이\")".to_owned())
        );
        assert_eq!((shown(index, "D4"), shown(index, "E4"), shown(index, "F4")), ("2".into(), "1".into(), "1".into()));
    }

    #[test]
    fn a_check_between_tables_converts_units_on_the_checks_sheet() {
        let d = doc(&format!(
            "<SECTION-1><TITLE>재 무 제 표</TITLE><TABLE><COLGROUP><COL/></COLGROUP><TBODY><TR><TD>재 무 상 태 표</TD></TR></TBODY></TABLE><P>(단위: 원)</P>{}</SECTION-1>\
             <SECTION-1><TITLE>주 석</TITLE><P>4. 현금</P><P>(단위: 천원)</P>{}</SECTION-1>",
            table(&["과 목", "주 석", "당 기", "전 기"], &[&["현금", "4", "1,500,000", "1,200,000"]]),
            table(&["구 분", "당기말", "전기말"], &[&["현금", "1,500", "1,200"]]),
        ));
        let (wb, _) = render(&d, &foot(&d)).unwrap();
        let listed = formulas(wb.sheets.last().unwrap());
        assert_eq!(listed[0], ("H4".to_owned(), "ROUND('재무상태표'!C9-(1000*SUM('주석4'!B8)),7)".to_owned()));
    }

    #[test]
    fn a_check_that_needs_judgment_gets_no_formula() {
        let d = doc(&table(&["구 분", "당기"], &[&["가", "1"], &["나", "주석 5"], &["합 계", "1"]]));
        let mut report = foot(&d);
        report.checks = crate::foot::evaluate(
            &d,
            &[crate::foot::Check {
                id: "a:1".into(),
                kind: crate::foot::Kind::Vertical,
                origin: Origin::Agent,
                rule: None,
                target: crate::foot::CellRef {
                    table: 1,
                    row: 4,
                    col: 2,
                    row_label: None,
                    col_label: None,
                    section: None,
                },
                terms: vec![crate::foot::Term { sign: 1, table: 1, rows: (2, 3), cols: (2, 2) }],
                tolerance: None,
                note: Some("n".into()),
            }],
        )
        .checks;
        let (wb, _) = render(&d, &report).unwrap();
        let checks = wb.sheets.last().unwrap();
        assert!(formulas(checks).is_empty());
        assert_eq!(shown(checks, "J4"), "판단 필요");
        assert_eq!(shown(checks, "K4"), "T1R3C2 is not a figure");
        assert!(formulas(&wb.sheets[1]).is_empty(), "nothing is placed in the table");
    }

    #[test]
    fn a_position_inside_a_span_is_read_at_the_span_s_first_cell() {
        // 합 계 spans both figure columns: its 6 is at the merge's first cell (B).
        let d = doc(
            "<TABLE><COLGROUP><COL/><COL/><COL/></COLGROUP><THEAD><TR><TH>구분</TH><TH>갑</TH><TH>을</TH></TR></THEAD><TBODY>\
             <TR><TD>가</TD><TD>1</TD><TD>2</TD></TR><TR><TD>나</TD><TD>1</TD><TD>2</TD></TR>\
             <TR><TD>합 계</TD><TD COLSPAN=\"2\">6</TD></TR></TBODY></TABLE>",
        );
        let check = crate::foot::Check {
            id: "a:1".into(),
            kind: crate::foot::Kind::Vertical,
            origin: Origin::Agent,
            rule: None,
            target: crate::foot::CellRef { table: 1, row: 4, col: 3, row_label: None, col_label: None, section: None },
            terms: vec![
                crate::foot::Term { sign: 1, table: 1, rows: (2, 3), cols: (2, 3) },
                crate::foot::Term { sign: -1, table: 1, rows: (4, 4), cols: (3, 3) },
            ],
            tolerance: None,
            note: Some("n".into()),
        };
        let mut report = foot(&d);
        report.checks = crate::foot::evaluate(&d, &[check]).checks;
        assert_eq!(report.checks[0].difference, Some(6.0), "6 - (1 + 2 + 1 + 2 - 6): the span is read once");
        let (wb, _) = render(&d, &report).unwrap();
        let listed = formulas(&wb.sheets[1]);
        // The target C7 is inside the B7:C7 merge; so is the second term.
        assert_eq!(listed[0].1, "ROUND(B7-(SUM(B5:C6)-SUM(B7)),12)");
    }

    #[test]
    fn a_difference_finer_than_six_places_still_shows() {
        // 0.0000004 is a real difference: the rounding keeps it.
        let d = doc(&table(&["구 분", "당기"], &[&["가", "0.0000004"], &["합 계", "0.0000008"]]));
        let mut report = foot(&d);
        report.checks = crate::foot::evaluate(
            &d,
            &[crate::foot::Check {
                id: "a:1".into(),
                kind: crate::foot::Kind::Vertical,
                origin: Origin::Agent,
                rule: None,
                target: crate::foot::CellRef {
                    table: 1,
                    row: 3,
                    col: 2,
                    row_label: None,
                    col_label: None,
                    section: None,
                },
                terms: vec![crate::foot::Term { sign: 1, table: 1, rows: (2, 2), cols: (2, 2) }],
                tolerance: None,
                note: Some("n".into()),
            }],
        )
        .checks;
        assert_eq!(report.checks[0].status, Status::Fail);
        let (wb, _) = render(&d, &report).unwrap();
        let cell = wb.sheets[1].cells.iter().find(|c| matches!(&c.value, Some(Value::Formula { .. }))).unwrap();
        assert!(
            matches!(&cell.value, Some(Value::Formula { result, .. }) if result == "0.0000004"),
            "{:?}",
            cell.value
        );
    }

    #[test]
    fn a_one_unit_difference_at_any_magnitude_fails_in_both() {
        let d = doc(&table(&["구 분", "당기"], &[&["가", "50,000,000,000,000"], &["합 계", "50,000,000,000,001"]]));
        let report = foot(&d);
        assert_eq!((report.unresolved[0].status, report.unresolved[0].difference), (Status::Fail, Some(1.0)));
        let mut report = report;
        report.checks = std::mem::take(&mut report.unresolved);
        let (wb, _) = render(&d, &report).unwrap();
        let listed = formulas(&wb.sheets[1]);
        assert_eq!(listed[0].1, "ROUND(B6-(SUM(B5)),0)", "rounded to the places the figures carry");
    }

    #[test]
    fn a_decimal_typed_into_an_integer_check_still_shows() {
        // Integer figures: the formula keeps places beyond 0, so an edit to 0.9 shows.
        let d = doc(&table(&["구 분", "당기"], &[&["가", "1"], &["합 계", "1"]]));
        let (wb, _) = render(&d, &foot(&d)).unwrap();
        let listed = formulas(&wb.sheets[1]);
        assert_eq!(listed[0].1, "ROUND(B6-(SUM(B5)),13)");
    }

    #[test]
    fn binary_noise_in_decimal_sums_shows_as_no_difference() {
        let d = doc(&table(&["구 분", "당기"], &[&["가", "0.1"], &["나", "0.2"], &["합 계", "0.3"]]));
        let report = foot(&d);
        assert_eq!(report.checks[0].status, Status::Pass);
        let (wb, _) = render(&d, &report).unwrap();
        let cell = wb.sheets[1].cells.iter().find(|c| matches!(&c.value, Some(Value::Formula { .. }))).unwrap();
        assert!(matches!(&cell.value, Some(Value::Formula { result, .. }) if result == "0"));
    }
}
