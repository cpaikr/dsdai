//! Footing: sum and cross-reference checks over a document's tables.
//!
//! Every check has one shape, `target = Σ sign × term`, where a term is a
//! rectangle of cells in one table addressed by `T/R/C`. `kind` only
//! describes a check; evaluation is the same for all. The CLI computes every
//! value here; an agent only states which cells should relate.
//!
//! Values are read with the xlsx export's typing ([`crate::amount`]), so a
//! check sees exactly the numbers the workbook holds. A check passes only
//! when its difference is 0, unless it states a tolerance. Tables in
//! different units are converted when both units are known from their
//! captions; otherwise the check needs judgment instead of being evaluated.

mod overlay;
mod propose;
mod render;

use std::collections::HashSet;

pub use overlay::{ChecksFile, Dismissal, Dismissed, Entry, Unapplied};
pub use propose::{Proposals, propose};
pub use render::{CHECKS_SHEET, render};
use serde::{Deserialize, Serialize};

use crate::amount;
use crate::doc::{BLOCK_TEXT_TAGS, Doc, Table, is_scaffold};
use crate::text::{collapse, node_text, node_text_outside_tables};
use crate::xml::NodeId;

// ---------------------------------------------------------------------------
// model

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "kebab-case")]
pub enum Kind {
    Vertical,
    Horizontal,
    Movement,
    Cross,
    Identity,
}

/// Who stated the check: a CLI proposer or the agent.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "kebab-case")]
pub enum Origin {
    Auto,
    Agent,
}

/// Which proposer stated an automatic check.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "kebab-case")]
pub enum Rule {
    /// A row labelled 합계, 소계, 총계, or 계 sums the rows above it.
    LabelTotal,
    /// A numbered or indented line sums the lines under it (a statement's
    /// subtotal column over its detail column included).
    Hierarchy,
    /// A statement line is a signed sum of the sibling lines above it.
    SignedSum,
    /// A closing row is the opening row plus the movements between them, or
    /// an opening row repeats the closing row above it.
    Movement,
    /// A column labelled as a total sums its row, or an unlabelled column
    /// sums the columns left of it in more than half its rows.
    ColumnTotal,
    /// A closing column is the opening column plus the movement columns.
    ColumnMovement,
    /// 자산총계 = 부채와자본총계, or 부채총계 + 자본총계, wherever those lines
    /// meet in one table; asserted from the labels, so a failure is a finding.
    Identity,
    /// A statement line's amounts are the same in a note it names, for both
    /// periods.
    StatementNote,
    /// The current period's opening balance repeats the prior period's
    /// closing balance (one table with both periods, or a pair of tables).
    PriorClosing,
}

/// One cell by `T/R/C`, with its row's account name, its column's header
/// text, and the section its table is in (the workbook sheet's title, such
/// as `재무상태표` or `4. 현금및현금성자산`). In a result they are the
/// document's labels now; in a checks file they are the labels an entry was
/// written against, verified before it applies.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct CellRef {
    pub table: usize,
    pub row: usize,
    pub col: usize,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub row_label: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub col_label: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub section: Option<String>,
}

impl CellRef {
    /// The cell with the document's current labels; none for a cell the
    /// document does not have.
    pub(crate) fn at(doc: &Doc, ctx: &Context, table: usize, row: usize, col: usize) -> Self {
        let (row_label, col_label) = match doc.table(table) {
            Ok(t) if doc.cell_at(t, row, col).is_ok() => (row_label(doc, t, row), col_label(doc, t, col)),
            _ => (None, None),
        };
        let section = ctx.sections.get(table.wrapping_sub(1)).cloned().flatten();
        Self { table, row, col, row_label, col_label, section }
    }

    pub fn position(&self) -> (usize, usize, usize) {
        (self.table, self.row, self.col)
    }
}

/// A signed sum over a rectangle of cells in one table (1-based, inclusive).
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(try_from = "TermJson", into = "TermJson")]
pub struct Term {
    /// +1 or -1.
    pub sign: i8,
    pub table: usize,
    pub rows: (usize, usize),
    pub cols: (usize, usize),
}

/// The JSON form of a term: `row` or `rows: [from, to]`, `col` or `cols`.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
struct TermJson {
    sign: i8,
    table: usize,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    row: Option<usize>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    rows: Option<[usize; 2]>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    col: Option<usize>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    cols: Option<[usize; 2]>,
}

fn span(one: Option<usize>, range: Option<[usize; 2]>, what: &str) -> Result<(usize, usize), String> {
    let (from, to) = match (one, range) {
        (Some(n), None) => (n, n),
        (None, Some([a, b])) => (a, b),
        _ => return Err(format!("a term needs exactly one of `{what}` or `{what}s`")),
    };
    if from == 0 || from > to {
        return Err(format!("a term's {what}s must run from 1 upward, got {from}..{to}"));
    }
    Ok((from, to))
}

impl TryFrom<TermJson> for Term {
    type Error = String;

    fn try_from(j: TermJson) -> Result<Self, String> {
        if j.sign != 1 && j.sign != -1 {
            return Err(format!("a term's sign is 1 or -1, got {}", j.sign));
        }
        if j.table == 0 {
            return Err("tables are numbered from 1".to_owned());
        }
        Ok(Self { sign: j.sign, table: j.table, rows: span(j.row, j.rows, "row")?, cols: span(j.col, j.cols, "col")? })
    }
}

impl From<Term> for TermJson {
    fn from(t: Term) -> Self {
        let (row, rows) =
            if t.rows.0 == t.rows.1 { (Some(t.rows.0), None) } else { (None, Some([t.rows.0, t.rows.1])) };
        let (col, cols) =
            if t.cols.0 == t.cols.1 { (Some(t.cols.0), None) } else { (None, Some([t.cols.0, t.cols.1])) };
        Self { sign: t.sign, table: t.table, row, rows, col, cols }
    }
}

/// How far a difference may be from 0 and still pass, in the target's unit.
#[derive(Debug, Clone, Copy, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub enum Tolerance {
    Absolute(f64),
    /// This much for every cell summed by the terms.
    PerTerm(f64),
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct Check {
    pub id: String,
    pub kind: Kind,
    pub origin: Origin,
    /// The proposer behind an automatic check.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub rule: Option<Rule>,
    pub target: CellRef,
    pub terms: Vec<Term>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub tolerance: Option<Tolerance>,
    /// Why the relation holds (agent checks).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub note: Option<String>,
}

// ---------------------------------------------------------------------------
// result

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "camelCase")]
pub enum Status {
    Pass,
    Fail,
    /// The check could not be evaluated as stated; `reason` says why.
    NeedsJudgment,
}

#[derive(Debug, Clone, PartialEq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct CheckResult {
    pub id: String,
    pub kind: Kind,
    pub origin: Origin,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub rule: Option<Rule>,
    pub status: Status,
    pub target: CellRef,
    pub terms: Vec<Term>,
    /// The target's value.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub actual: Option<f64>,
    /// Σ sign × term, in the target's unit.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub expected: Option<f64>,
    /// actual − expected.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub difference: Option<f64>,
    /// The tolerance that applied (0 unless the check states one).
    pub allowed: f64,
    /// Cells summed by the terms (a spanning cell counts once).
    pub cells: usize,
    /// Σ |values| of the target and every summed cell, which bounds their
    /// binary rounding; for rendering, not the result.
    #[serde(skip)]
    pub magnitude: f64,
    /// The target table's unit caption, when found.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub unit: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub reason: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub note: Option<String>,
}

#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct Counts {
    pub pass: usize,
    pub fail: usize,
    pub needs_judgment: usize,
}

#[derive(Debug, Clone, PartialEq, Serialize)]
pub struct Footing {
    pub checks: Vec<CheckResult>,
    pub counts: Counts,
}

/// A labelled total with nothing to sum, or a statement amount its notes do
/// not show.
#[derive(Debug, Clone, PartialEq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct Uncovered {
    pub kind: Kind,
    #[serde(flatten)]
    pub cell: CellRef,
}

#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct ReportCounts {
    pub pass: usize,
    pub fail: usize,
    pub needs_judgment: usize,
    pub unresolved: usize,
    pub candidates: usize,
    pub uncovered: usize,
    pub dismissed: usize,
    pub moved: usize,
    pub stale: usize,
}

/// Proposed and agent checks with their results, the totals no structure
/// explains (each with its closest candidate, evaluated), statement amounts
/// found in their notes for one period (candidates, evaluated), what nothing
/// could cover, and how a checks file applied: proposals it dismissed, and
/// entries whose labels moved or are gone, which do not apply.
#[derive(Debug, Clone, PartialEq, Serialize)]
pub struct Report {
    pub checks: Vec<CheckResult>,
    pub unresolved: Vec<CheckResult>,
    pub candidates: Vec<CheckResult>,
    pub uncovered: Vec<Uncovered>,
    pub dismissed: Vec<Dismissed>,
    pub moved: Vec<Unapplied>,
    pub stale: Vec<Unapplied>,
    pub counts: ReportCounts,
    /// For the envelope's warnings, not the result.
    #[serde(skip)]
    pub warnings: Vec<crate::check::Issue>,
}

/// Propose every check the document's structure supports and evaluate them.
pub fn foot(doc: &Doc) -> Report {
    foot_with(doc, &ChecksFile::default())
}

/// Propose, apply a checks file over the proposals, and evaluate the result.
pub fn foot_with(doc: &Doc, file: &ChecksFile) -> Report {
    let ctx = Context::new(doc);
    let mut proposals = propose(doc);
    let applied = overlay::apply(doc, &ctx, file, &mut proposals);
    let checks = evaluate_in(doc, &ctx, &proposals.checks);
    let unresolved = evaluate_in(doc, &ctx, &proposals.unresolved).checks;
    let candidates = evaluate_in(doc, &ctx, &proposals.candidates).checks;
    let uncovered: Vec<Uncovered> = proposals
        .uncovered
        .iter()
        .map(|&(kind, t, row, col)| Uncovered { kind, cell: CellRef::at(doc, &ctx, t, row, col) })
        .collect();
    let counts = ReportCounts {
        pass: checks.counts.pass,
        fail: checks.counts.fail,
        needs_judgment: checks.counts.needs_judgment,
        unresolved: unresolved.len(),
        candidates: candidates.len(),
        uncovered: uncovered.len(),
        dismissed: applied.dismissed.len(),
        moved: applied.moved.len(),
        stale: applied.stale.len(),
    };
    Report {
        checks: checks.checks,
        unresolved,
        candidates,
        uncovered,
        dismissed: applied.dismissed,
        moved: applied.moved,
        stale: applied.stale,
        counts,
        warnings: proposals.warnings,
    }
}

// ---------------------------------------------------------------------------
// evaluation

/// What evaluation and label verification read once per document: each
/// table's unit and section.
pub(crate) struct Context {
    units: Vec<Option<Unit>>,
    sections: Vec<Option<String>>,
}

impl Context {
    pub(crate) fn new(doc: &Doc) -> Self {
        Self { units: table_units(doc), sections: table_sections(doc) }
    }

    pub(crate) fn section(&self, table: usize) -> Option<&str> {
        self.sections.get(table.wrapping_sub(1))?.as_deref()
    }
}

/// The title of the sheet each table is on in the workbook model: where the
/// table is in the document, which outlasts renumbering. None for every
/// table when the model cannot be built (an Excel limit).
pub fn table_sections(doc: &Doc) -> Vec<Option<String>> {
    let mut out = vec![None; doc.tables.len()];
    if let Ok(wb) = crate::sheets::workbook(doc) {
        for p in &wb.placements {
            out[p.table - 1] = Some(wb.sheets[p.sheet].title.clone());
        }
    }
    out
}

/// Evaluate every check against the document, in order.
pub fn evaluate(doc: &Doc, checks: &[Check]) -> Footing {
    evaluate_in(doc, &Context::new(doc), checks)
}

fn evaluate_in(doc: &Doc, ctx: &Context, checks: &[Check]) -> Footing {
    let results: Vec<CheckResult> = checks.iter().map(|c| evaluate_one(doc, ctx, c)).collect();
    let mut counts = Counts::default();
    for r in &results {
        match r.status {
            Status::Pass => counts.pass += 1,
            Status::Fail => counts.fail += 1,
            Status::NeedsJudgment => counts.needs_judgment += 1,
        }
    }
    Footing { checks: results, counts }
}

fn evaluate_one(doc: &Doc, ctx: &Context, check: &Check) -> CheckResult {
    let units = &ctx.units;
    let t = &check.target;
    let mut result = CheckResult {
        id: check.id.clone(),
        kind: check.kind,
        origin: check.origin,
        rule: check.rule,
        status: Status::NeedsJudgment,
        target: CellRef::at(doc, ctx, t.table, t.row, t.col),
        terms: check.terms.clone(),
        actual: None,
        expected: None,
        difference: None,
        allowed: 0.0,
        cells: 0,
        magnitude: 0.0,
        unit: units.get(t.table.wrapping_sub(1)).cloned().flatten().map(|u| u.text),
        reason: None,
        note: check.note.clone(),
    };
    match compute(doc, units, check) {
        Ok((actual, expected, cells, magnitude)) => {
            let allowed = match check.tolerance {
                None => 0.0,
                Some(Tolerance::Absolute(a)) => a.abs(),
                Some(Tolerance::PerTerm(p)) => p.abs() * cells as f64,
            };
            let difference = actual - expected;
            let slack = rounding_slack(magnitude);
            result.status = if difference.abs() <= allowed + slack { Status::Pass } else { Status::Fail };
            result.actual = Some(actual);
            result.expected = Some(expected);
            result.difference = Some(difference);
            result.allowed = allowed;
            result.cells = cells;
            result.magnitude = magnitude;
        }
        Err(reason) => result.reason = Some(reason),
    }
    result
}

/// How far binary rounding of decimal figures can move a sum whose values
/// add up to `magnitude` in absolute terms: a few units in the last place,
/// so a difference of one displayed unit is never absorbed (it is under
/// 0.02 at 10¹²).
pub(crate) fn rounding_slack(magnitude: f64) -> f64 {
    64.0 * f64::EPSILON * magnitude.max(1.0)
}

/// (actual, expected, cells summed, Σ |values|), or why the check cannot be
/// evaluated.
fn compute(doc: &Doc, units: &[Option<Unit>], check: &Check) -> Result<(f64, f64, usize, f64), String> {
    let t = &check.target;
    let actual = match read(doc, t.table, t.row, t.col)? {
        Read::Number(v) => v,
        Read::Blank => return Err(format!("the target T{}R{}C{} is blank", t.table, t.row, t.col)),
    };
    if check.terms.is_empty() {
        return Err("the check has no terms".to_owned());
    }
    // The JSON form rejects these; a check built through the API is held to the same rules.
    for term in &check.terms {
        let ordered = |(a, b): (usize, usize)| a >= 1 && a <= b;
        if !matches!(term.sign, 1 | -1) || term.table == 0 || !ordered(term.rows) || !ordered(term.cols) {
            return Err(format!("a term needs a sign of 1 or -1 and rows and columns from 1 upward, got {term:?}"));
        }
    }
    let mut expected = 0.0;
    let mut magnitude = actual.abs();
    let mut cells = 0;
    for term in &check.terms {
        let scale = scale(units, term.table, t.table)?;
        let mut seen = HashSet::new();
        for row in term.rows.0..=term.rows.1 {
            for col in term.cols.0..=term.cols.1 {
                let table = doc.table(term.table).map_err(|e| e.message)?;
                let cell = doc.cell_at(table, row, col).map_err(|e| e.message)?;
                // A spanning cell covers several positions but holds one figure.
                if !seen.insert((cell.row, cell.col)) {
                    continue;
                }
                cells += 1;
                if let Read::Number(v) = read(doc, term.table, cell.row, cell.col)? {
                    expected += f64::from(term.sign) * v * scale;
                    magnitude += (v * scale).abs();
                }
            }
        }
    }
    Ok((actual, expected, cells, magnitude))
}

enum Read {
    Number(f64),
    Blank,
}

/// The figure in a cell: a number, a blank (counted as 0 in a sum), or an
/// error naming the cell when it holds text.
fn read(doc: &Doc, t: usize, row: usize, col: usize) -> Result<Read, String> {
    let table = doc.table(t).map_err(|e| e.message)?;
    let cell = doc.cell_at(table, row, col).map_err(|e| e.message)?;
    let text = node_text_outside_tables(&doc.tree, cell.node);
    if text.trim().is_empty() {
        return Ok(Read::Blank);
    }
    amount::parse(&text)
        .map(|a| Read::Number(a.value))
        .ok_or_else(|| format!("T{t}R{}C{} is not a figure", cell.row, cell.col))
}

/// How to convert a value from table `from`'s unit to table `to`'s.
pub(crate) fn scale(units: &[Option<Unit>], from: usize, to: usize) -> Result<f64, String> {
    if from == to {
        return Ok(1.0);
    }
    let unit = |t: usize| units.get(t.wrapping_sub(1)).cloned().flatten();
    match (unit(from), unit(to)) {
        (Some(Unit { factor: Some(fa), .. }), Some(Unit { factor: Some(fb), .. })) => Ok(fa / fb),
        // A unit that is not money compares only with itself, and only when
        // it is one the tool knows (a caption it cannot read is not one).
        (Some(a), Some(b)) if a.text == b.text && PLAIN_UNITS.contains(&a.text.as_str()) => Ok(1.0),
        (a, b) => Err(format!(
            "T{from} ({}) and T{to} ({}) cannot be compared without a known unit for each",
            a.map_or_else(|| "unit not found".to_owned(), |u| u.text),
            b.map_or_else(|| "unit not found".to_owned(), |u| u.text),
        )),
    }
}

// ---------------------------------------------------------------------------
// units

/// Units that are not money but compare with themselves: counts of shares,
/// people, and items, and foreign currencies.
const PLAIN_UNITS: [&str; 12] = ["주", "천주", "명", "개", "건", "회", "EA", "USD", "천USD", "EUR", "JPY", "CNY"];

/// A table's unit as its caption states it (`천원`), and its size in won
/// when it is a currency unit.
#[derive(Debug, Clone, PartialEq)]
pub struct Unit {
    pub text: String,
    pub factor: Option<f64>,
}

/// `(단위: 천원)` and similar captions; the first unit after the colon counts.
pub fn unit_in_text(text: &str) -> Option<Unit> {
    let compacted: String = text.chars().filter(|c| !c.is_whitespace()).collect();
    let after = &compacted[compacted.find("단위")? + "단위".len()..];
    // A caption names its unit after a colon; a sentence that mentions 단위 does not.
    let after = after.strip_prefix([':', '：'])?;
    let unit: String = after.chars().take_while(|&c| !matches!(c, ')' | '）' | ',' | '，' | ';' | ']')).collect();
    if unit.is_empty() {
        return None;
    }
    let factor = match unit.as_str() {
        "원" => Some(1.0),
        "천원" => Some(1e3),
        "백만원" => Some(1e6),
        "억원" => Some(1e8),
        "십억원" => Some(1e9),
        _ => None,
    };
    Some(Unit { text: unit, factor })
}

fn unit_in_table(doc: &Doc, table: &Table) -> Option<Unit> {
    table.cells.iter().find_map(|c| unit_in_text(&node_text_outside_tables(&doc.tree, c.node)))
}

/// Each table's unit (index `T# - 1`): its own caption, else the caption in
/// the block just before it (a unit line or a small caption table, as DART
/// documents place them). A table nested in a cell only has its own.
pub fn table_units(doc: &Doc) -> Vec<Option<Unit>> {
    struct Walk<'a> {
        doc: &'a Doc,
        units: Vec<Option<Unit>>,
        previous: Option<Unit>,
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
                    let own = unit_in_table(doc, table);
                    self.units[table.index - 1] = own.clone().or_else(|| self.previous.take());
                    self.previous = own;
                    for nested in tree.iter_tag(child, "TABLE").into_iter().filter(|&n| n != child) {
                        if let Some(t) = doc.table_for_node(nested) {
                            self.units[t.index - 1] = unit_in_table(doc, t);
                        }
                    }
                } else if BLOCK_TEXT_TAGS.contains(&tag) {
                    let text = node_text(tree, child, false);
                    if !text.trim().is_empty() {
                        self.previous = unit_in_text(&text);
                    }
                } else {
                    self.walk(child);
                }
            }
        }
    }
    let mut w = Walk { doc, units: vec![None; doc.tables.len()], previous: None };
    w.walk(doc.root());
    w.units
}

// ---------------------------------------------------------------------------
// labels

fn cell_text(doc: &Doc, table: &Table, row: usize, col: usize) -> Option<String> {
    let cell = doc.cell_at(table, row, col).ok()?;
    let text = collapse(&node_text_outside_tables(&doc.tree, cell.node));
    (!text.is_empty()).then_some(text)
}

/// The first text in the row that is not a figure: the account name.
pub fn row_label(doc: &Doc, table: &Table, row: usize) -> Option<String> {
    (1..=table.ncols()).filter_map(|c| cell_text(doc, table, row, c)).find(|t| amount::parse(t).is_none())
}

/// The header cells above a column (`TH` rows from the top, joined with ` / `),
/// or the first row's text when the table has no header cells. A unit
/// caption heading the whole table (`(단위: 원)`) is not part of a label.
pub fn col_label(doc: &Doc, table: &Table, col: usize) -> Option<String> {
    let mut parts: Vec<String> = Vec::new();
    for row in 1..=table.rows.len() {
        let Ok(cell) = doc.cell_at(table, row, col) else { break };
        if doc.tag(cell.node) != "TH" {
            break;
        }
        if let Some(text) = cell_text(doc, table, row, col)
            && parts.last() != Some(&text)
            && unit_in_text(&text).is_none()
        {
            parts.push(text);
        }
    }
    if parts.is_empty() {
        return cell_text(doc, table, 1, col).filter(|t| amount::parse(t).is_none() && unit_in_text(t).is_none());
    }
    Some(parts.join(" / "))
}

#[cfg(test)]
mod tests {
    use super::testing::{doc, table};
    use super::*;

    fn unit_table(unit: &str) -> String {
        format!("<TABLE><COLGROUP><COL/></COLGROUP><TBODY><TR><TD>(단위: {unit})</TD></TR></TBODY></TABLE>")
    }

    fn check(id: &str, target: (usize, usize, usize), terms: Vec<Term>) -> Check {
        Check {
            id: id.to_owned(),
            kind: Kind::Vertical,
            origin: Origin::Agent,
            rule: None,
            target: CellRef {
                table: target.0,
                row: target.1,
                col: target.2,
                row_label: None,
                col_label: None,
                section: None,
            },
            terms,
            tolerance: None,
            note: None,
        }
    }

    fn term(sign: i8, table: usize, rows: (usize, usize), col: usize) -> Term {
        Term { sign, table, rows, cols: (col, col) }
    }

    const BALANCE: &[&[&str]] =
        &[&["현금", "1,000", "700"], &["매출채권", "500", ""], &["재고", "(200)", "-"], &["자산총계", "1,300", "700"]];

    #[test]
    fn vertical_sums_pass_fail_and_count_blanks_as_zero() {
        let d = doc(&table(&["과 목", "당기", "전기"], BALANCE));
        let checks = [
            check("T1.R5.C2", (1, 5, 2), vec![term(1, 1, (2, 4), 2)]),
            check("T1.R5.C3", (1, 5, 3), vec![term(1, 1, (2, 4), 3)]),
            check("wrong", (1, 5, 2), vec![term(1, 1, (2, 3), 2)]),
        ];
        let f = evaluate(&d, &checks);
        assert_eq!(f.checks[0].status, Status::Pass);
        assert_eq!((f.checks[0].actual, f.checks[0].expected, f.checks[0].cells), (Some(1300.0), Some(1300.0), 3));
        assert_eq!(f.checks[1].status, Status::Pass, "a blank and a nil dash count as 0");
        assert_eq!(f.checks[2].status, Status::Fail);
        assert_eq!(f.checks[2].difference, Some(-200.0));
        assert_eq!(f.counts, Counts { pass: 2, fail: 1, needs_judgment: 0 });
        let target = &f.checks[0].target;
        assert_eq!((target.row_label.as_deref(), target.col_label.as_deref()), (Some("자산총계"), Some("당기")));
    }

    #[test]
    fn tolerance_is_zero_unless_stated() {
        let d = doc(&table(&["과 목", "당기"], &[&["가", "100"], &["나", "201"], &["계", "300"]]));
        let mut c = check("c", (1, 4, 2), vec![term(1, 1, (2, 3), 2)]);
        assert_eq!(evaluate(&d, &[c.clone()]).checks[0].status, Status::Fail);
        c.tolerance = Some(Tolerance::PerTerm(1.0));
        let r = &evaluate(&d, &[c.clone()]).checks[0];
        assert_eq!((r.status, r.allowed), (Status::Pass, 2.0));
        c.tolerance = Some(Tolerance::Absolute(0.5));
        assert_eq!(evaluate(&d, &[c]).checks[0].status, Status::Fail);
    }

    #[test]
    fn text_in_a_term_or_a_blank_target_needs_judgment() {
        let d = doc(&table(&["과 목", "당기"], &[&["가", "100"], &["나", "주석 5"], &["계", ""]]));
        let f = evaluate(
            &d,
            &[
                check("text", (1, 2, 2), vec![term(1, 1, (3, 3), 2)]),
                check("blank", (1, 4, 2), vec![term(1, 1, (2, 2), 2)]),
                check("missing", (1, 9, 2), vec![term(1, 1, (2, 2), 2)]),
                check("no table", (1, 2, 2), vec![term(1, 7, (2, 2), 2)]),
            ],
        );
        let reasons: Vec<(Status, &str)> =
            f.checks.iter().map(|c| (c.status, c.reason.as_deref().unwrap_or(""))).collect();
        assert_eq!(reasons[0], (Status::NeedsJudgment, "T1R3C2 is not a figure"));
        assert_eq!(reasons[1], (Status::NeedsJudgment, "the target T1R4C2 is blank"));
        assert_eq!(reasons[2].0, Status::NeedsJudgment);
        assert_eq!(reasons[3].0, Status::NeedsJudgment);
        assert_eq!(f.counts.needs_judgment, 4);
    }

    #[test]
    fn movement_schedules_and_footnoted_amounts_sum() {
        // opening + increase − decrease = closing, with a footnoted figure.
        let d =
            doc(&table(&["구 분", "기초", "증가", "감소", "기말"], &[&["토지", "1,000", "300 (*1)", "100", "1,200"]]));
        let mut c =
            check("move", (1, 2, 5), vec![term(1, 1, (2, 2), 2), term(1, 1, (2, 2), 3), term(-1, 1, (2, 2), 4)]);
        c.kind = Kind::Movement;
        let r = &evaluate(&d, &[c]).checks[0];
        assert_eq!((r.status, r.expected), (Status::Pass, Some(1200.0)));
    }

    #[test]
    fn a_spanning_cell_is_summed_once() {
        let d = doc(
            "<TABLE><COLGROUP><COL/><COL/></COLGROUP><TBODY><TR><TD>가</TD><TD ROWSPAN=\"2\">50</TD></TR><TR><TD>나</TD></TR><TR><TD>계</TD><TD>50</TD></TR></TBODY></TABLE>",
        );
        let r = &evaluate(&d, &[check("span", (1, 3, 2), vec![term(1, 1, (1, 2), 2)])]).checks[0];
        assert_eq!((r.status, r.cells), (Status::Pass, 1));
    }

    #[test]
    fn a_term_built_through_the_api_obeys_the_json_rules() {
        let d = doc(&table(&["과 목", "당기"], &[&["가", "0"], &["계", "0"]]));
        let mut c = check("c", (1, 3, 2), vec![term(1, 1, (3, 2), 2)]);
        let r = &evaluate(&d, &[c.clone()]).checks[0];
        assert_eq!(r.status, Status::NeedsJudgment, "an empty row range sums nothing");
        c.terms = vec![term(2, 1, (2, 2), 2)];
        assert_eq!(evaluate(&d, &[c]).checks[0].status, Status::NeedsJudgment);
    }

    #[test]
    fn captions_naming_an_unknown_unit_do_not_compare() {
        let d = doc(&format!(
            "<P>(단위: 미상)</P>{}<P>(단위: 미상)</P>{}<P>(단위: 주)</P>{}<P>(단위: 주)</P>{}",
            table(&["구 분", "당기"], &[&["가", "10"]]),
            table(&["구 분", "당기"], &[&["가", "10"]]),
            table(&["구 분", "당기"], &[&["가", "10"]]),
            table(&["구 분", "당기"], &[&["가", "10"]]),
        ));
        let cross = |id: &str, target: usize, from: usize| check(id, (target, 2, 2), vec![term(1, from, (2, 2), 2)]);
        let f = evaluate(&d, &[cross("unknown", 2, 1), cross("shares", 4, 3)]);
        assert_eq!((f.checks[0].status, f.checks[1].status), (Status::NeedsJudgment, Status::Pass));
    }

    #[test]
    fn units_come_from_captions_and_convert_across_tables() {
        let d = doc(&format!(
            "{}{}<P>(단위: 천원)</P>{}<P>설명</P>{}",
            unit_table("원"),
            table(&["과 목", "당기"], &[&["현금", "1,500,000"]]),
            table(&["구 분", "당기"], &[&["보통예금", "1,500"]]),
            table(&["구 분", "당기"], &[&["기타", "1,500"]]),
        ));
        let units = table_units(&d);
        let texts: Vec<Option<&str>> = units.iter().map(|u| u.as_ref().map(|u| u.text.as_str())).collect();
        assert_eq!(texts, [Some("원"), Some("원"), Some("천원"), None]);
        let cross = |id: &str, from: usize| {
            let mut c = check(id, (2, 2, 2), vec![term(1, from, (2, 2), 2)]);
            c.kind = Kind::Cross;
            c
        };
        let f = evaluate(&d, &[cross("converted", 3), cross("unknown", 4)]);
        assert_eq!((f.checks[0].status, f.checks[0].expected), (Status::Pass, Some(1_500_000.0)));
        assert_eq!(f.checks[0].unit.as_deref(), Some("원"));
        assert_eq!(f.checks[1].status, Status::NeedsJudgment);
        assert!(f.checks[1].reason.as_deref().unwrap().contains("unit not found"));
    }

    #[test]
    fn unit_captions() {
        let unit = |s: &str| unit_in_text(s).map(|u| (u.text, u.factor));
        assert_eq!(unit("(단위: 원)"), Some(("원".into(), Some(1.0))));
        assert_eq!(unit("(단위 : 천원)"), Some(("천원".into(), Some(1e3))));
        assert_eq!(unit("（단위：백만원, 주）"), Some(("백만원".into(), Some(1e6))));
        assert_eq!(unit("(단위: 주)"), Some(("주".into(), None)));
        assert_eq!(unit("단위 없음이라는 문장"), None);
        assert_eq!(unit("(단위:원)"), Some(("원".into(), Some(1.0))));
        assert_eq!(unit("(금액: 원)"), None);
    }

    #[test]
    fn a_unit_caption_row_is_not_part_of_a_column_label() {
        let d = doc(
            "<TABLE><COLGROUP><COL/><COL/></COLGROUP><THEAD><TR><TH COLSPAN=\"2\">(단위: 원)</TH></TR><TR><TH>구분</TH><TH>당기</TH></TR></THEAD><TBODY><TR><TD>현금</TD><TD>10</TD></TR></TBODY></TABLE>",
        );
        assert_eq!(col_label(&d, d.table(1).unwrap(), 2).as_deref(), Some("당기"));
    }

    #[test]
    fn header_rows_label_columns() {
        let d = doc(
            "<TABLE><COLGROUP><COL/><COL/><COL/></COLGROUP><THEAD><TR><TH ROWSPAN=\"2\">구분</TH><TH COLSPAN=\"2\">당기말</TH></TR><TR><TH>취득원가</TH><TH>장부금액</TH></TR></THEAD><TBODY><TR><TD>토지</TD><TD>10</TD><TD>9</TD></TR></TBODY></TABLE>",
        );
        let t = d.table(1).unwrap();
        assert_eq!(col_label(&d, t, 3).as_deref(), Some("당기말 / 장부금액"));
        assert_eq!(col_label(&d, t, 1).as_deref(), Some("구분"));
        assert_eq!(row_label(&d, t, 3).as_deref(), Some("토지"));
    }

    #[test]
    fn checks_read_and_write_their_json_form() {
        let json = r#"{"id":"T13.R3.C4","kind":"vertical","origin":"auto",
            "target":{"table":13,"row":3,"col":4,"rowLabel":"유동자산"},
            "terms":[{"sign":1,"table":13,"rows":[4,8],"col":4},{"sign":-1,"table":2,"row":1,"cols":[2,3]}],
            "tolerance":{"perTerm":1}}"#;
        let c: Check = serde_json::from_str(json).unwrap();
        assert_eq!(c.terms[0], Term { sign: 1, table: 13, rows: (4, 8), cols: (4, 4) });
        assert_eq!(c.terms[1], Term { sign: -1, table: 2, rows: (1, 1), cols: (2, 3) });
        assert_eq!(c.tolerance, Some(Tolerance::PerTerm(1.0)));
        let back = serde_json::to_value(&c).unwrap();
        assert_eq!(back["terms"][0], serde_json::json!({"sign": 1, "table": 13, "rows": [4, 8], "col": 4}));
        assert_eq!(serde_json::from_value::<Check>(back).unwrap(), c);
        for bad in [
            r#"{"sign":2,"table":1,"row":1,"col":1}"#,
            r#"{"sign":1,"table":1,"row":1,"rows":[1,2],"col":1}"#,
            r#"{"sign":1,"table":1,"rows":[3,2],"col":1}"#,
            r#"{"sign":1,"table":0,"row":1,"col":1}"#,
            r#"{"sign":1,"table":1,"col":1}"#,
            r#"{"sign":1,"table":1,"row":1,"col":1,"extra":true}"#,
        ] {
            assert!(serde_json::from_str::<Term>(bad).is_err(), "{bad}");
        }
    }

    #[test]
    fn results_serialize_for_the_process_contract() {
        let d = doc(&table(&["과 목", "당기"], &[&["가", "1"], &["계", "1"]]));
        let f = evaluate(&d, &[check("c", (1, 3, 2), vec![term(1, 1, (2, 2), 2)])]);
        let v = serde_json::to_value(&f).unwrap();
        assert_eq!(v["counts"], serde_json::json!({"pass": 1, "fail": 0, "needsJudgment": 0}));
        assert_eq!(v["checks"][0]["status"], "pass");
        assert_eq!(v["checks"][0]["target"]["rowLabel"], "계");
        assert!(v["checks"][0].get("reason").is_none());
    }
}

/// Synthetic documents for the footing tests.
#[cfg(test)]
pub(crate) mod testing {
    use crate::doc::Doc;

    pub fn doc(body: &str) -> Doc {
        let xml = format!(
            "<?xml version=\"1.0\" encoding=\"utf-8\"?>\n<DOCUMENT xmlns:xsi=\"http://www.w3.org/2001/XMLSchema-instance\" xsi:noNamespaceSchemaLocation=\"dart4.xsd\"><BODY>{body}</BODY></DOCUMENT>"
        );
        Doc::parse(xml.into_bytes()).unwrap()
    }

    /// A table: header cells, then rows of cells, where a cell `text|n`
    /// spans n columns.
    pub fn table(header: &[&str], rows: &[&[&str]]) -> String {
        let cell = |tag: &str, text: &str| match text.rsplit_once('|') {
            Some((t, n)) => format!("<{tag} COLSPAN=\"{n}\">{t}</{tag}>"),
            None => format!("<{tag}>{text}</{tag}>"),
        };
        let width: usize = header.iter().map(|h| h.rsplit_once('|').map_or(1, |(_, n)| n.parse().unwrap())).sum();
        let mut t = format!("<TABLE><COLGROUP>{}</COLGROUP><THEAD><TR>", "<COL/>".repeat(width));
        for h in header {
            t.push_str(&cell("TH", h));
        }
        t.push_str("</TR></THEAD><TBODY>");
        for row in rows {
            t.push_str("<TR>");
            for c in *row {
                t.push_str(&cell("TD", c));
            }
            t.push_str("</TR>");
        }
        t.push_str("</TBODY></TABLE>");
        t
    }
}
