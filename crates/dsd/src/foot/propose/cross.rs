//! Proposers across tables: statement lines against their notes, and each
//! period's opening balance against the prior period's closing balance.
//!
//! A statement line names its notes (`5,6,32` in a 주석 column, or `(주4)`
//! in its label). When those notes hold the line's amounts for both periods,
//! the match is recorded as a check; a match for one period (including a
//! line that shows only one) is a candidate for the agent, and a line whose
//! notes show none of its amounts is uncovered. Nothing here is reported as an error unless the
//! relation is an identity: an opening balance always repeats the prior
//! period's closing balance.

use std::collections::HashMap;

use super::{Grid, Proposals, V, balance_word, equal, key};
use crate::check::Issue;
use crate::compare::note_heading;
use crate::doc::{BLOCK_TEXT_TAGS, Doc, is_scaffold};
use crate::foot::{Check, Kind, Rule, Term, col_label, scale, table_units, unit_in_text};
use crate::sheets::{SheetKind, workbook};
use crate::text::{compact, node_text, node_text_outside_tables};
use crate::xml::NodeId;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Period {
    Current,
    Prior,
}

/// The period a heading names: 당기 (당분기, 당반기, `(당)`) or 전기; None
/// when it names neither or both.
fn period(text: &str) -> Option<Period> {
    let w = compact(text);
    let prior = ["전기", "(전)", "전분기", "전반기", "전년"].iter().any(|p| w.contains(p));
    let current = ["당기", "(당)", "당분기", "당반기", "당년"].iter().any(|p| w.contains(p));
    match (current, prior) {
        (true, false) => Some(Period::Current),
        (false, true) => Some(Period::Prior),
        _ => None,
    }
}

fn named(text: &str) -> Option<(Period, bool)> {
    period(text).map(|p| (p, interim(text)))
}

/// An interim period (분기, 반기): its closing balance is not the next
/// period's opening balance, which follows the prior year end.
fn interim(text: &str) -> bool {
    text.contains("분기") || text.contains("반기")
}

impl Grid<'_> {
    /// The current and prior period groups, named by their headings; with
    /// `positional`, also the first and second of exactly two unnamed groups
    /// (a statement lists the current period first, but two unnamed columns
    /// of a note are usually two items, not two periods).
    fn periods(&self, positional: bool) -> Option<(usize, usize)> {
        let named: Vec<Option<Period>> =
            self.groups.iter().map(|g| col_label(self.doc, self.table, g[0]).as_deref().and_then(period)).collect();
        let find = |p: Period| {
            let mut it = named.iter().enumerate().filter(|(_, n)| **n == Some(p)).map(|(i, _)| i);
            let first = it.next();
            first.filter(|_| it.next().is_none())
        };
        match (find(Period::Current), find(Period::Prior)) {
            (Some(c), Some(p)) => Some((c, p)),
            _ if positional && self.groups.len() == 2 && named.iter().all(Option::is_none) => Some((0, 1)),
            _ => None,
        }
    }

    /// target = one cell, of this table or another.
    fn cell_check(&self, rule: Rule, kind: Kind, target: (usize, usize), term: (usize, usize, usize)) -> Check {
        let term = Term { sign: 1, table: term.0, rows: (term.1, term.1), cols: (term.2, term.2) };
        self.check(rule, kind, "x", target, vec![term])
    }
}

/// Note numbers a statement line refers to: a 주석 column's `5,6,32`, or
/// `(주4,24,33)` / `(주석 5)` in the label.
fn note_refs(grid: &Grid, r: usize) -> Vec<u32> {
    let numbers = |s: &str| -> Vec<u32> {
        s.split(|c: char| !c.is_ascii_digit())
            .filter(|d| (1..=2).contains(&d.len()))
            .filter_map(|d| d.parse().ok())
            .collect()
    };
    for c in 1..=grid.table.ncols() {
        if col_label(grid.doc, grid.table, c).is_some_and(|l| compact(&l).contains("주석"))
            && let Some(cell) = grid.table.cell_at_grid(r - 1, c - 1)
            && cell.row == r
        {
            let refs = numbers(&node_text_outside_tables(&grid.doc.tree, cell.node));
            if !refs.is_empty() {
                return refs;
            }
        }
    }
    let label = grid.label[r - 1].as_deref().map(compact).unwrap_or_default();
    label
        .match_indices("(주")
        .filter_map(|(i, _)| label[i..].split_once(')').map(|(inner, _)| numbers(inner)))
        .flatten()
        .collect()
}

/// A note cell holding a statement amount, ranked by how surely it is the
/// line's figure: the same label, then a total row.
#[derive(Debug, Clone, Copy)]
struct Hit {
    table: usize,
    row: usize,
    col: usize,
    score: u8,
}

/// Statement lines against the notes they name.
pub(super) fn statement_notes(doc: &Doc, out: &mut Proposals) {
    let wb = match workbook(doc) {
        Ok(wb) => wb,
        Err(e) => {
            out.warnings.push(Issue::warning(
                "statement-notes-skipped",
                format!("statement lines were not matched with their notes: {}", e.message),
            ));
            return;
        }
    };
    let units = table_units(doc);
    let mut statements = Vec::new();
    let mut notes: HashMap<u32, Vec<usize>> = HashMap::new();
    for p in &wb.placements {
        let sheet = &wb.sheets[p.sheet];
        match sheet.kind {
            SheetKind::Statement => statements.push(p.table),
            SheetKind::Note => {
                if let Some((n, _)) = note_heading(&sheet.title) {
                    notes.entry(n).or_default().push(p.table);
                }
            }
            _ => {}
        }
    }
    let mut grids: HashMap<usize, Grid> = HashMap::new();
    for &s in &statements {
        let grid = Grid::new(doc, &doc.tables[s - 1]);
        let Some((cur, prior)) = grid.periods(true) else { continue };
        for r in grid.data_rows() {
            let refs = note_refs(&grid, r);
            if refs.is_empty() {
                continue;
            }
            let wanted: Vec<(usize, usize, f64)> = [cur, prior]
                .into_iter()
                .filter_map(|g| match grid.in_group(&grid.groups[g], r) {
                    Some((c, V::Num(x))) if x != 0.0 => Some((g, c, x)),
                    _ => None,
                })
                .collect();
            // A note the document does not hold (or holds without tables) is
            // nothing to compare against.
            if wanted.is_empty() || !refs.iter().any(|n| notes.contains_key(n)) {
                continue;
            }
            let line = grid.label[r - 1].as_deref().map(key).unwrap_or_default();
            // Every note cell carrying each period's amount, in the statement's unit.
            let mut hits: Vec<Vec<Hit>> = vec![Vec::new(); wanted.len()];
            for t in refs.iter().filter_map(|n| notes.get(n)).flatten().copied() {
                let g = grids.entry(t).or_insert_with(|| Grid::new(doc, &doc.tables[t - 1]));
                // An unknown unit compares as is; the check then needs judgment.
                let f = scale(&units, t, s).unwrap_or(1.0);
                for rr in g.data_rows() {
                    let label = g.label[rr - 1].as_deref().map(key).unwrap_or_default();
                    let score = if !line.is_empty() && label == line { 2 } else { u8::from(g.rank(rr).is_some()) };
                    for &c in &g.figure_cols {
                        let V::Num(x) = g.at(rr, c) else { continue };
                        for (i, &(_, _, amount)) in wanted.iter().enumerate() {
                            if x != 0.0 && equal(x * f, amount) {
                                hits[i].push(Hit { table: t, row: rr, col: c, score });
                            }
                        }
                    }
                }
            }
            let emit = |hit: &Hit, i: usize| {
                let mut check =
                    grid.cell_check(Rule::StatementNote, Kind::Cross, (r, wanted[i].1), (hit.table, hit.row, hit.col));
                check.id = format!("{}/T{}", check.id, hit.table);
                check
            };
            let best =
                |hs: &[Hit]| hs.iter().copied().max_by_key(|h| (h.score, std::cmp::Reverse((h.table, h.row, h.col))));
            if wanted.len() == 2 && hits.iter().all(|h| !h.is_empty()) {
                // Prefer one note row holding both periods side by side.
                let pairs = hits[0].iter().flat_map(|a| hits[1].iter().map(move |b| (*a, *b)));
                let side_by_side = pairs
                    .filter(|(a, b)| a.table == b.table && a.row == b.row && a.col != b.col)
                    .max_by_key(|(a, b)| (a.score.max(b.score), std::cmp::Reverse((a.table, a.row))));
                let (a, b) = side_by_side.unwrap_or_else(|| (best(&hits[0]).unwrap(), best(&hits[1]).unwrap()));
                out.checks.push(emit(&a, 0));
                out.checks.push(emit(&b, 1));
            } else if hits.iter().any(|h| !h.is_empty()) {
                for (i, hs) in hits.iter().enumerate() {
                    if let Some(h) = best(hs) {
                        out.candidates.push(emit(&h, i));
                    }
                }
            } else {
                out.uncovered.extend(wanted.iter().map(|&(_, c, _)| (Kind::Cross, s, r, c)));
            }
        }
    }
}

/// Opening balances of the current period against the prior period's
/// closing balances: within one table with a 당기 and a 전기 column, and
/// between a pair of movement tables, one per period.
pub(super) fn prior_closings(doc: &Doc, out: &mut Proposals) {
    let grids: Vec<Option<Grid>> = doc
        .tables
        .iter()
        .map(|t| {
            let g = (!t.oversized && !t.rows.is_empty() && t.ncols() > 0).then(|| Grid::new(doc, t))?;
            (!g.figure_cols.is_empty()).then_some(g)
        })
        .collect();
    for g in grids.iter().flatten() {
        within(g, out);
    }
    let periods = table_periods(doc);
    let mut paired = vec![false; grids.len()];
    for (i, a) in grids.iter().enumerate() {
        let Some(a) = a else { continue };
        if paired[i] {
            continue;
        }
        let Some((pa, interim_a)) = periods[i] else { continue };
        // The other period's table: the nearest one after it naming the other
        // period with the same columns (당기 취득원가 with 전기 취득원가, even
        // when 당기 감가상각누계액 sits between them).
        let partner = (i + 1..grids.len().min(i + 7)).find(|&j| {
            !paired[j]
                && periods[j].is_some_and(|(p, _)| p != pa)
                && grids[j].as_ref().is_some_and(|b| shape(b) == shape(a))
        });
        let Some(j) = partner else { continue };
        let (Some(b), Some((_, interim_b))) = (&grids[j], periods[j]) else { continue };
        paired[j] = true;
        let (current, prior, prior_interim) = if pa == Period::Current { (a, b, interim_b) } else { (b, a, interim_a) };
        if prior_interim {
            continue;
        }
        across_columns(current, prior, out);
        across_rows(current, prior, out);
    }
}

/// A table's figure columns by label, to pair one period's table with the
/// other's.
fn shape(g: &Grid) -> Vec<String> {
    g.figure_cols.iter().map(|&c| col_label(g.doc, g.table, c).map(|l| key(&l)).unwrap_or_default()).collect()
}

/// `기초` in the current period's column = `기말` in the prior period's.
fn within(g: &Grid, out: &mut Proposals) {
    let Some((cur, prior)) = g.periods(false) else { return };
    if col_label(g.doc, g.table, g.groups[prior][0]).is_some_and(|l| interim(&l)) {
        return;
    }
    let top = g.top();
    let rows: Vec<usize> = g.data_rows().filter(|&r| g.participates(r) && g.level(r) == top).collect();
    for (i, &o) in rows.iter().enumerate() {
        if !g.is_opening(o) {
            continue;
        }
        let Some(&c) = rows[i + 1..].iter().take_while(|&&r| !g.is_opening(r)).find(|&&r| g.is_closing(r)) else {
            continue;
        };
        let (Some((oc, V::Num(_))), Some((cc, V::Num(_)))) =
            (g.in_group(&g.groups[cur], o), g.in_group(&g.groups[prior], c))
        else {
            continue;
        };
        out.checks.push(g.cell_check(Rule::PriorClosing, Kind::Movement, (o, oc), (g.table.index, c, cc)));
    }
}

/// Movement tables with 기초 and 기말 columns: each row's opening in the
/// current table = the same row's closing in the prior table.
fn across_columns(current: &Grid, prior: &Grid, out: &mut Proposals) {
    let col = |g: &Grid, word: &str| {
        g.figure_cols.iter().copied().find(|&c| col_label(g.doc, g.table, c).is_some_and(|l| balance_word(&l, word)))
    };
    let (Some(open), Some(close)) = (col(current, "기초"), col(prior, "기말")) else { return };
    let labels = |g: &Grid| unique(g.data_rows().filter_map(|r| Some((g.label[r - 1].as_deref().map(key)?, r))));
    let (mine, theirs) = (labels(current), labels(prior));
    let mut rows: Vec<(usize, usize)> = mine.iter().filter_map(|(k, &r)| Some((r, *theirs.get(k)?))).collect();
    rows.sort_unstable();
    for (r, pr) in rows {
        if matches!(current.at(r, open), V::Num(_)) && matches!(prior.at(pr, close), V::Num(_)) {
            out.checks.push(current.cell_check(
                Rule::PriorClosing,
                Kind::Movement,
                (r, open),
                (prior.table.index, pr, close),
            ));
        }
    }
}

/// Movement tables with 기초 and 기말 rows: each column's opening in the
/// current table = the same column's closing in the prior table.
fn across_rows(current: &Grid, prior: &Grid, out: &mut Proposals) {
    let row = |g: &Grid, opening: bool| {
        g.data_rows().find(|&r| g.participates(r) && if opening { g.is_opening(r) } else { g.is_closing(r) })
    };
    let (Some(open), Some(close)) = (row(current, true), row(prior, false)) else { return };
    let labels = |g: &Grid| {
        unique(g.figure_cols.iter().filter_map(|&c| Some((col_label(g.doc, g.table, c).map(|l| key(&l))?, c))))
    };
    let (mine, theirs) = (labels(current), labels(prior));
    let mut cols: Vec<(usize, usize)> = mine.iter().filter_map(|(k, &c)| Some((c, *theirs.get(k)?))).collect();
    cols.sort_unstable();
    for (c, pc) in cols {
        if matches!(current.at(open, c), V::Num(_)) && matches!(prior.at(close, pc), V::Num(_)) {
            out.checks.push(current.cell_check(
                Rule::PriorClosing,
                Kind::Movement,
                (open, c),
                (prior.table.index, close, pc),
            ));
        }
    }
}

/// Labels that occur once, with their row or column; a repeated label (two
/// 소계 rows) cannot say which of its rows pairs with the other table's.
fn unique(labelled: impl Iterator<Item = (String, usize)>) -> std::collections::BTreeMap<String, usize> {
    let mut seen: HashMap<String, Option<usize>> = HashMap::new();
    for (k, i) in labelled.filter(|(k, _)| !k.is_empty()) {
        seen.entry(k).and_modify(|e| *e = None).or_insert(Some(i));
    }
    seen.into_iter().filter_map(|(k, i)| Some((k, i?))).collect()
}

/// Each table's period and whether it is an interim one, from its own header
/// row or the caption just before it (`<당기>`, `(2) 전분기`); None when
/// neither names one.
fn table_periods(doc: &Doc) -> Vec<Option<(Period, bool)>> {
    struct Walk<'a> {
        doc: &'a Doc,
        periods: Vec<Option<(Period, bool)>>,
        previous: Option<(Period, bool)>,
    }
    impl Walk<'_> {
        fn walk(&mut self, node: NodeId) {
            let doc = self.doc;
            let tree = &doc.tree;
            for &child in tree.children(node) {
                let tag = tree.tag(child);
                if is_scaffold(tag) || tag == "DOCUMENT-HEADER" {
                    continue;
                }
                if let Some(table) = doc.table_for_node(child) {
                    let first_row: String = table
                        .cells
                        .iter()
                        .filter(|c| c.row == 1)
                        .map(|c| node_text_outside_tables(tree, c.node))
                        .collect::<Vec<_>>()
                        .join(" ");
                    if table.rows.len() == 1 {
                        // A one-row table is a caption for the next table; a
                        // unit caption keeps the period named before it.
                        if named(&first_row).is_some() || unit_in_text(&first_row).is_none() {
                            self.previous = named(&first_row);
                        }
                        continue;
                    }
                    self.periods[table.index - 1] = named(&first_row).or(self.previous.take());
                } else if BLOCK_TEXT_TAGS.contains(&tag) {
                    let text = node_text(tree, child, false);
                    let line = text.lines().rev().find(|l| !l.trim().is_empty()).unwrap_or("");
                    if !line.is_empty() && (named(line).is_some() || unit_in_text(line).is_none()) {
                        self.previous = named(line);
                    }
                } else {
                    self.walk(child);
                }
            }
        }
    }
    let mut w = Walk { doc, periods: vec![None; doc.tables.len()], previous: None };
    w.walk(doc.root());
    w.periods
}

#[cfg(test)]
mod tests {
    use crate::doc::Doc;
    use crate::foot::testing::{doc, table};
    use crate::foot::{Report, Status, foot};

    /// A balance sheet naming notes 4 to 7, and those notes.
    fn report() -> Doc {
        let statement = table(
            &["과 목", "주 석", "당 기", "전 기"],
            &[
                &["현금및현금성자산", "4", "1,000", "900"],
                &["매출채권", "5", "500", "400"],
                &["재고자산", "6", "300", "200"],
                &["선급금", "7", "80", "70"],
            ],
        );
        let notes = [
            "<P>4. 현금및현금성자산</P><P>(단위: 원)</P>",
            &table(
                &["구 분", "당기말", "전기말"],
                &[&["보통예금", "600", "500"], &["정기예금", "400", "400"], &["합 계", "1,000", "900"]],
            ),
            "<P>5. 매출채권</P><P>(단위: 원)</P>",
            &table(&["구 분", "당기말", "전기말"], &[&["매출채권", "500", "450"]]),
            "<P>6. 재고자산</P><P>(단위: 원)</P>",
            &table(
                &["구 분", "당 기", "전 기"],
                &[&["기초", "200", "150"], &["  매입", "100", "50"], &["기말", "300", "200"]],
            ),
            "<P>7. 선급금</P><P>(단위: 원)</P>",
            &table(&["구 분", "당기말", "전기말"], &[&["선급금", "8", "7"]]),
        ]
        .concat();
        doc(&format!(
            "<SECTION-1><TITLE>재 무 제 표</TITLE><TABLE><COLGROUP><COL/></COLGROUP><TBODY><TR><TD>재 무 상 태 표</TD></TR></TBODY></TABLE><P>(단위: 원)</P>{statement}</SECTION-1>\
             <SECTION-1><TITLE>주 석</TITLE>{notes}</SECTION-1>"
        ))
    }

    fn ids(results: &[crate::foot::CheckResult]) -> Vec<(String, Status)> {
        results.iter().map(|c| (c.id.clone(), c.status)).collect()
    }

    fn cross(r: &Report) -> Vec<(String, Status)> {
        ids(&r.checks).into_iter().filter(|(id, _)| id.starts_with("x:")).collect()
    }

    #[test]
    fn statement_lines_match_their_notes_for_every_period() {
        let r = foot(&report());
        assert_eq!(
            cross(&r),
            [
                ("x:T2.R2.C3/T3".to_owned(), Status::Pass),
                ("x:T2.R2.C4/T3".to_owned(), Status::Pass),
                ("x:T2.R4.C3/T5".to_owned(), Status::Pass),
                ("x:T2.R4.C4/T5".to_owned(), Status::Pass),
                // 6. 재고자산 also opens the current period with the prior closing.
                ("x:T5.R2.C2".to_owned(), Status::Pass),
            ]
        );
        let cash: Vec<(usize, usize, usize)> = r.checks
            [r.checks.iter().position(|c| c.id == "x:T2.R2.C3/T3").unwrap()..][..2]
            .iter()
            .map(|c| (c.terms[0].table, c.terms[0].rows.0, c.terms[0].cols.0))
            .collect();
        assert_eq!(cash, [(3, 4, 2), (3, 4, 3)], "the note's total row, side by side");
        // 5. 매출채권 shows the current amount only: a candidate, not an error.
        assert_eq!(ids(&r.candidates), [("x:T2.R3.C3/T4".to_owned(), Status::Pass)]);
        // 7. 선급금 shows neither amount.
        let uncovered: Vec<(usize, usize, usize)> =
            r.uncovered.iter().map(|u| (u.cell.table, u.cell.row, u.cell.col)).collect();
        assert_eq!(uncovered, [(2, 5, 3), (2, 5, 4)]);
    }

    #[test]
    fn a_note_in_another_unit_matches_after_conversion() {
        let d = doc(&format!(
            "<SECTION-1><TITLE>재 무 제 표</TITLE><TABLE><COLGROUP><COL/></COLGROUP><TBODY><TR><TD>재 무 상 태 표</TD></TR></TBODY></TABLE><P>(단위: 원)</P>{}</SECTION-1>\
             <SECTION-1><TITLE>주 석</TITLE><P>4. 현금</P><P>(단위: 천원)</P>{}</SECTION-1>",
            table(&["과 목", "주 석", "당 기", "전 기"], &[&["현금", "4", "1,500,000", "1,200,000"]]),
            table(&["구 분", "당기말", "전기말"], &[&["현금", "1,500", "1,200"]]),
        ));
        assert_eq!(
            cross(&foot(&d)),
            [("x:T2.R2.C3/T3".to_owned(), Status::Pass), ("x:T2.R2.C4/T3".to_owned(), Status::Pass)]
        );
    }

    #[test]
    fn paired_movement_tables_open_with_the_prior_closing() {
        let movement = |rows: &[&[&str]]| table(&["구 분", "기초", "취득", "기말"], rows);
        let d = doc(&format!(
            "<P>(1) 당기</P><P>(단위: 원)</P>{}<P>(2) 전기</P><P>(단위: 원)</P>{}<P>(1) 당분기</P><P>(단위: 원)</P>{}<P>(2) 전분기</P><P>(단위: 원)</P>{}",
            movement(&[&["토지", "100", "10", "110"], &["건물", "50", "5", "55"]]),
            movement(&[&["토지", "90", "10", "100"], &["건물", "40", "5", "45"]]),
            movement(&[&["토지", "100", "10", "110"]]),
            movement(&[&["토지", "95", "5", "100"]]),
        ));
        let r = foot(&d);
        let prior: Vec<(String, Status)> = cross(&r).into_iter().filter(|(id, _)| !id.contains('/')).collect();
        assert_eq!(
            prior,
            [("x:T1.R2.C2".to_owned(), Status::Pass), ("x:T1.R3.C2".to_owned(), Status::Fail)],
            "a prior interim period's closing is not compared"
        );
        let building = &r.checks.iter().find(|c| c.id == "x:T1.R3.C2").unwrap();
        assert_eq!((building.terms[0].table, building.terms[0].rows.0, building.terms[0].cols.0), (2, 3, 4));
        assert_eq!(building.difference, Some(5.0));
    }

    #[test]
    fn two_unnamed_columns_of_a_note_are_items_not_periods() {
        let d = doc(&format!(
            "<P>(단위: 원)</P>{}",
            table(
                &["구 분", "판매보증충당부채", "소송충당부채"],
                &[&["기초", "100", "200"], &["전입", "10", "20"], &["기말", "110", "220"]]
            ),
        ));
        assert!(cross(&foot(&d)).is_empty());
    }

    #[test]
    fn a_repeated_label_is_not_paired_across_tables() {
        let movement = |land: &str| {
            table(
                &["구 분", "기초", "취득", "기말"],
                &[
                    &["토지", land, "10", "110"],
                    &["소계", "30", "0", "30"],
                    &["특허권", "5", "0", "5"],
                    &["소계", "150", "0", "150"],
                ],
            )
        };
        let d = doc(&format!(
            "<P>(1) 당기</P><P>(단위: 원)</P>{}<P>(2) 전기</P><P>(단위: 원)</P>{}",
            movement("110"),
            movement("100"),
        ));
        // 토지 and 특허권 pair; neither 소계 does, though each would match one.
        let paired: Vec<(String, Status)> = cross(&foot(&d)).into_iter().filter(|(id, _)| !id.contains('/')).collect();
        assert_eq!(paired, [("x:T1.R2.C2".to_owned(), Status::Pass), ("x:T1.R4.C2".to_owned(), Status::Pass)]);
    }
}
