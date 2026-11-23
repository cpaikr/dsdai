//! Proposers: deterministic candidate checks, each verified arithmetically.
//!
//! A proposer finds a cell that should be a total: by its label (`합계`,
//! `소계`, `총계`, `계`, a closing balance), by its place in a numbered or
//! indented hierarchy, or by a statement's detail/subtotal column pair. It
//! then tries the structures that could make the cell a total and keeps the
//! one that holds in the most period columns, so a true difference in one
//! period still surfaces as a failed check. A labelled total that no
//! structure explains is **unresolved** (reported with its closest
//! candidate); one with nothing to sum is **uncovered**.
//!
//! Structures that need no label to be found are kept only when they hold
//! with at least two non-zero parts: a signed chain of statement lines in
//! every period, a column summing the columns left of it in more than half
//! its rows. Coincidences stay out.

mod cross;

use std::collections::HashSet;

use crate::amount;
use crate::doc::{Doc, Table};
use crate::text::{collapse, compact, node_text_outside_tables};

use super::{CellRef, Check, Kind, Origin, Rule, Term, col_label};

/// Longest chain of sibling lines a signed sum may combine.
const MAX_CHAIN: usize = 8;

#[derive(Debug, Default)]
pub struct Proposals {
    pub checks: Vec<Check>,
    /// Labelled totals no structure explains, each with its closest candidate.
    pub unresolved: Vec<Check>,
    /// Statement amounts found in their notes for only some periods: offered
    /// to the agent, never reported as errors.
    pub candidates: Vec<Check>,
    /// Labelled totals with nothing to sum, and statement amounts their
    /// notes do not show: (kind, table, row, col).
    pub uncovered: Vec<(Kind, usize, usize, usize)>,
    /// Why part of the document was not proposed over.
    pub warnings: Vec<crate::check::Issue>,
}

/// Every proposal for the document, in table order.
pub fn propose(doc: &Doc) -> Proposals {
    let mut out = Proposals::default();
    for table in &doc.tables {
        if table.oversized || table.rows.is_empty() || table.ncols() == 0 {
            continue;
        }
        let grid = Grid::new(doc, table);
        if grid.figure_cols.is_empty() {
            continue;
        }
        grid.vertical(&mut out);
        let explained = grid.identities(&mut out);
        grid.horizontal(&mut out, &explained);
    }
    cross::statement_notes(doc, &mut out);
    cross::prior_closings(doc, &mut out);
    let mut seen = HashSet::new();
    for c in out.checks.iter_mut().chain(out.unresolved.iter_mut()).chain(out.candidates.iter_mut()) {
        if !seen.insert(c.id.clone()) {
            let mut n = 2;
            while !seen.insert(format!("{}#{n}", c.id)) {
                n += 1;
            }
            c.id = format!("{}#{n}", c.id);
        }
    }
    out
}

// ---------------------------------------------------------------------------
// the table as figures

#[derive(Debug, Clone, Copy, PartialEq)]
enum V {
    Num(f64),
    Blank,
    Text,
    /// A position a spanning cell covers but does not start at.
    Covered,
    /// A position no cell covers (a short row); the evaluator cannot read it,
    /// so it is never summed or skipped over.
    Missing,
}

/// Signed rows (vertical) or columns (horizontal).
type Cand = Vec<(i8, usize)>;

struct Grid<'a> {
    doc: &'a Doc,
    table: &'a Table,
    nrows: usize,
    /// `[row-1][col-1]`.
    v: Vec<Vec<V>>,
    /// First row holding a figure in a data cell (1-based).
    first_data: usize,
    figure_cols: Vec<usize>,
    /// Period columns: one figure column, or a detail/subtotal pair `[left, right]`.
    groups: Vec<Vec<usize>>,
    /// Per row: the raw text of its first own label cell.
    label: Vec<Option<String>>,
    /// Per row: every own text cell, whitespace collapsed (for keywords).
    words: Vec<Vec<String>>,
    /// `[row-1][col-1]`: the figure is a lone nil dash.
    dash: Vec<Vec<bool>>,
}

impl<'a> Grid<'a> {
    fn new(doc: &'a Doc, table: &'a Table) -> Self {
        let nrows = table.rows.len();
        let ncols = table.ncols();
        let mut v = vec![vec![V::Blank; ncols]; nrows];
        let mut label = vec![None; nrows];
        let mut words = vec![Vec::new(); nrows];
        let mut first_data = nrows + 1;
        let mut dash = vec![vec![false; ncols]; nrows];
        for r in 1..=nrows {
            for c in 1..=ncols {
                let Some(cell) = table.cell_at_grid(r - 1, c - 1) else {
                    v[r - 1][c - 1] = V::Missing;
                    continue;
                };
                if cell.row != r || cell.col != c {
                    v[r - 1][c - 1] = V::Covered;
                    continue;
                }
                let raw = node_text_outside_tables(&doc.tree, cell.node);
                if raw.trim().is_empty() {
                    continue;
                }
                // A percentage is a figure that never adds up: a rate row
                // (평균유효세율) under a column of amounts is not one of them.
                let figure = amount::parse(&raw).filter(|a| !a.format.contains('%'));
                if let Some(a) = figure {
                    v[r - 1][c - 1] = V::Num(a.value);
                    dash[r - 1][c - 1] = raw.trim() == "-";
                    if doc.tag(cell.node) != "TH" {
                        first_data = first_data.min(r);
                    }
                } else if amount::parse(&raw).is_some() {
                    v[r - 1][c - 1] = V::Text;
                } else {
                    v[r - 1][c - 1] = V::Text;
                    if label[r - 1].is_none() {
                        label[r - 1] = Some(raw.clone());
                    }
                    words[r - 1].push(collapse(&raw));
                }
            }
        }
        let mut g =
            Self { doc, table, nrows, v, first_data, figure_cols: Vec::new(), groups: Vec::new(), label, words, dash };
        g.figure_cols = (1..=ncols).filter(|&c| g.is_figure_col(c)).collect();
        g.groups = g.period_groups();
        g
    }

    fn at(&self, r: usize, c: usize) -> V {
        self.v[r - 1][c - 1]
    }

    fn data_rows(&self) -> std::ops::RangeInclusive<usize> {
        self.first_data..=self.nrows
    }

    /// A column of figures that add up: more figures than text (percentages
    /// count as text) below the header, and not labelled as note references (`주석`)
    /// or as a rate, price, average, count of issues, or date.
    fn is_figure_col(&self, c: usize) -> bool {
        if col_label(self.doc, self.table, c).is_some_and(|l| !additive(&l)) {
            return false;
        }
        let (mut nums, mut texts) = (0, 0);
        for r in self.data_rows() {
            match self.at(r, c) {
                V::Num(_) => nums += 1,
                V::Text => texts += 1,
                V::Blank | V::Covered | V::Missing => {}
            }
        }
        nums > 0 && nums >= texts
    }

    /// Adjacent figure columns under one header cell where no row has a
    /// figure in both form a detail/subtotal pair; the rest stand alone.
    fn period_groups(&self) -> Vec<Vec<usize>> {
        let mut groups = Vec::new();
        let mut i = 0;
        while i < self.figure_cols.len() {
            let c = self.figure_cols[i];
            let next = self.figure_cols.get(i + 1).copied();
            if let Some(d) = next.filter(|&d| d == c + 1 && self.is_pair(c, d)) {
                groups.push(vec![c, d]);
                i += 2;
            } else {
                groups.push(vec![c]);
                i += 1;
            }
        }
        groups
    }

    fn is_pair(&self, c: usize, d: usize) -> bool {
        // One period heading over exactly these two columns (a unit caption
        // spanning the whole table does not make a pair).
        let shared_header = (1..self.first_data).any(|r| {
            let (a, b) = (self.table.cell_at_grid(r - 1, c - 1), self.table.cell_at_grid(r - 1, d - 1));
            matches!((a, b), (Some(a), Some(b)) if a == b && a.col == c && a.colspan == 2)
        });
        let num = |r: usize, c: usize| matches!(self.at(r, c), V::Num(x) if x != 0.0);
        shared_header
            && self.data_rows().any(|r| num(r, c))
            && self.data_rows().any(|r| num(r, d))
            && !self.data_rows().any(|r| num(r, c) && num(r, d))
    }

    /// The row's figure in a period group: which column holds it, and its value.
    /// A nil dash in the other column of a pair is a placeholder, so a
    /// non-zero figure wins over it.
    fn in_group(&self, g: &[usize], r: usize) -> Option<(usize, V)> {
        let mut found = None;
        for &c in g {
            match self.at(r, c) {
                V::Num(x) if x != 0.0 => return Some((c, V::Num(x))),
                v @ (V::Num(_) | V::Text | V::Missing) if found.is_none() => found = Some((c, v)),
                _ => {}
            }
        }
        found
    }

    fn num(&self, g: &[usize], r: usize) -> Option<f64> {
        match self.in_group(g, r) {
            Some((_, V::Num(x))) => Some(x),
            _ => None,
        }
    }

    /// A row with a figure in any period group.
    fn participates(&self, r: usize) -> bool {
        self.groups.iter().any(|g| self.num(g, r).is_some())
    }

    /// A row of text only below the header: a section heading inside the table.
    fn is_heading(&self, r: usize) -> bool {
        !self.participates(r) && self.label[r - 1].is_some()
    }

    /// Hierarchy level: lower is higher. A pair's subtotal column ranks above
    /// its detail column; then numbering style, then indentation.
    fn level(&self, r: usize) -> (u8, u16) {
        let pos = self
            .groups
            .iter()
            .find_map(|g| (g.len() == 2).then(|| self.in_group(g, r)).flatten().filter(|(_, v)| matches!(v, V::Num(_))))
            .map_or(0, |(c, _)| u8::from(self.groups.iter().any(|g| g.len() == 2 && g[0] == c)));
        (pos, self.label[r - 1].as_deref().map_or(0, numbering))
    }

    /// The highest level any row with figures has.
    fn top(&self) -> (u8, u16) {
        self.data_rows().filter(|&r| self.participates(r)).map(|r| self.level(r)).min().unwrap_or((0, 0))
    }

    fn rank(&self, r: usize) -> Option<u8> {
        self.words[r - 1].iter().filter_map(|w| total_rank(w)).max()
    }

    fn is_opening(&self, r: usize) -> bool {
        self.words[r - 1].iter().map(|w| compact(w)).any(|w| w.contains("기초") || w.ends_with("초)"))
    }

    fn is_closing(&self, r: usize) -> bool {
        self.words[r - 1].iter().map(|w| compact(w)).any(|w| w.contains("기말") || w.ends_with("말)"))
    }

    fn sign_word(&self, r: usize) -> i8 {
        self.label[r - 1].as_deref().map_or(1, expense_sign)
    }

    // -----------------------------------------------------------------------
    // vertical: totals down a column

    fn vertical(&self, out: &mut Proposals) {
        for t in self.data_rows() {
            if !self.participates(t) {
                continue;
            }
            // A closing row is a total only at the table's top level; an
            // indented one (기말제품재고액 in a cost-of-sales schedule) is a part.
            let closing = self.is_closing(t) && self.level(t) == self.top();
            let labelled = self.rank(t).is_some() || closing;
            // An opening balance heads the movements under it; it sums nothing.
            let children = if self.is_opening(t) { Vec::new() } else { self.children(t) };
            // A breakdown implied by numbering or by a statement's subtotal
            // column is reported when it fails; one implied only by
            // indentation is kept only when it holds.
            let strong = children.iter().any(|&r| self.level(r).0 > self.level(t).0)
                || self.label[t - 1].as_deref().is_some_and(|l| (1..=6).contains(&numbering(l)));
            let mut cands: Vec<(Rule, Kind, Cand, bool)> = Vec::new();
            if !children.is_empty() {
                let plus: Cand = children.iter().map(|&r| (1, r)).collect();
                let minus: Cand = children.iter().map(|&r| (-1, r)).collect();
                cands.push((Rule::Hierarchy, Kind::Vertical, plus, false));
                cands.push((Rule::Hierarchy, Kind::Vertical, minus, false));
            }
            if closing {
                for c in self.movement(t) {
                    cands.push((Rule::Movement, Kind::Movement, c, false));
                }
                // Movements grouped under 소계 rows sum like a labelled total.
                if self.rank(t).is_none() {
                    for c in self.label_totals(t, 2) {
                        cands.push((Rule::Movement, Kind::Movement, c, false));
                    }
                }
            }
            if let Some(rank) = self.rank(t) {
                for c in self.label_totals(t, rank) {
                    cands.push((Rule::LabelTotal, Kind::Vertical, c, false));
                }
            }
            if self.is_opening(t)
                && let Some(p) = (self.first_data..t).rev().find(|&r| self.participates(r))
                && self.is_closing(p)
                && continues(self.label[p - 1].as_deref().unwrap_or(""), self.label[t - 1].as_deref().unwrap_or(""))
            {
                cands.push((Rule::Movement, Kind::Movement, vec![(1, p)], false));
            }
            for c in self.chains(t) {
                cands.push((Rule::SignedSum, Kind::Vertical, c, true));
            }
            cands.retain(|(_, _, c, _)| !c.is_empty());
            let mut seen: Vec<Cand> = Vec::new();
            cands.retain(|(_, _, c, _)| {
                let new = !seen.contains(c);
                seen.push(c.clone());
                new
            });
            let needed = labelled || (!children.is_empty() && strong);
            if cands.is_empty() {
                if labelled {
                    for g in &self.groups {
                        if let Some((c, V::Num(_))) = self.in_group(g, t) {
                            out.uncovered.push((Kind::Vertical, self.table.index, t, c));
                        }
                    }
                }
                continue;
            }
            self.choose(t, &cands, needed, out);
        }
    }

    /// Pick the candidate holding in the most period groups and emit it for
    /// every group where the target has a figure; when none holds and the
    /// total is implied, report the closest candidate as unresolved.
    fn choose(&self, t: usize, cands: &[(Rule, Kind, Cand, bool)], needed: bool, out: &mut Proposals) {
        let mut best: Option<((usize, usize), usize)> = None; // ((non-trivial, holding), candidate)
        for (i, (_, _, cand, strict)) in cands.iter().enumerate() {
            let groups = self.applicable(t, cand);
            let holding: Vec<&&Vec<usize>> = groups.iter().filter(|g| self.holds(g, t, cand)).collect();
            let nontrivial = holding.iter().filter(|g| !self.all_zero(g, t, cand)).count();
            if *strict {
                let nonzero = groups.iter().any(|g| self.num(g, t).is_some_and(|x| x != 0.0));
                let substantive = groups.iter().all(|g| self.nonzero_terms(g, cand) >= 2);
                if groups.is_empty() || holding.len() < groups.len() || !nonzero || !substantive {
                    continue;
                }
            }
            // 0 = 0 proves nothing unless the structure holds everywhere.
            if nontrivial == 0 && (holding.is_empty() || holding.len() < groups.len()) {
                continue;
            }
            let score = (nontrivial, holding.len());
            if best.is_none_or(|(s, _)| score > s) {
                best = Some((score, i));
            }
        }
        if let Some((_, i)) = best {
            let (rule, kind, cand, _) = &cands[i];
            for g in self.emitted(t, cand) {
                out.checks.push(self.vertical_check(g, t, (*rule, *kind, "v"), cand));
            }
            return;
        }
        if !needed {
            return;
        }
        let closest = cands
            .iter()
            .filter(|(_, _, cand, strict)| !strict && !self.applicable(t, cand).is_empty())
            .min_by(|a, b| self.miss(t, &a.2).total_cmp(&self.miss(t, &b.2)));
        if let Some((rule, kind, cand, _)) = closest {
            for g in self.emitted(t, cand) {
                out.unresolved.push(self.vertical_check(g, t, (*rule, *kind, "v"), cand));
            }
        }
    }

    /// The period groups where `cand` can make `t` a total: the target is a
    /// figure, the parts hold no text, and the target is not a lone dash over
    /// parts that do not net to zero (a total left out, not a total of zero).
    fn applicable(&self, t: usize, cand: &Cand) -> Vec<&Vec<usize>> {
        self.groups
            .iter()
            .filter(|g| {
                let Some((c, V::Num(_))) = self.in_group(g, t) else { return false };
                self.sum(g, cand).is_some_and(|s| !self.dash[t - 1][c - 1] || equal(s, 0.0))
            })
            .collect()
    }

    /// The period groups a chosen structure is stated for: those where it
    /// applies, and those where a part holds text, which evaluation then
    /// reports as needing judgment instead of the period disappearing.
    fn emitted(&self, t: usize, cand: &Cand) -> Vec<&Vec<usize>> {
        self.groups
            .iter()
            .filter(|g| {
                let Some((c, V::Num(_))) = self.in_group(g, t) else { return false };
                let dash = self.dash[t - 1][c - 1];
                self.sum(g, cand).map_or(!dash, |s| !dash || equal(s, 0.0))
            })
            .collect()
    }

    fn all_zero(&self, g: &[usize], t: usize, cand: &Cand) -> bool {
        self.num(g, t) == Some(0.0) && self.nonzero_terms(g, cand) == 0
    }

    fn sum(&self, g: &[usize], cand: &Cand) -> Option<f64> {
        let mut total = 0.0;
        for &(sign, r) in cand {
            match self.in_group(g, r) {
                Some((_, V::Num(x))) => total += f64::from(sign) * x,
                Some(_) => return None,
                None => {}
            }
        }
        Some(total)
    }

    fn holds(&self, g: &[usize], t: usize, cand: &Cand) -> bool {
        match (self.num(g, t), self.sum(g, cand)) {
            (Some(a), Some(e)) => equal(a, e),
            _ => false,
        }
    }

    fn miss(&self, t: usize, cand: &Cand) -> f64 {
        self.applicable(t, cand)
            .iter()
            .map(|g| match (self.num(g, t), self.sum(g, cand)) {
                (Some(a), Some(e)) => (a - e).abs(),
                _ => f64::INFINITY,
            })
            .sum()
    }

    fn nonzero_terms(&self, g: &[usize], cand: &Cand) -> usize {
        cand.iter().filter(|&&(_, r)| self.num(g, r).is_some_and(|x| x != 0.0)).count()
    }

    /// The rows directly under `t` in the hierarchy: following rows ranked
    /// below it, up to the next row at its level or above, or a heading.
    fn children(&self, t: usize) -> Vec<usize> {
        let level = self.level(t);
        let mut range = Vec::new();
        for r in t + 1..=self.nrows {
            if self.is_heading(r) {
                break;
            }
            if !self.participates(r) {
                continue;
            }
            if self.level(r) <= level || self.rank(r).is_some() {
                break;
            }
            range.push(r);
        }
        top_level(self, range)
    }

    /// Candidates for a labelled total of `rank` (1 소계, 2 합계/계, 3 총계).
    fn label_totals(&self, t: usize, rank: u8) -> Vec<Cand> {
        let plus = |rows: Vec<usize>| -> Cand { rows.into_iter().map(|r| (1, r)).collect() };
        let mut cands = Vec::new();
        // The rows under the row-spanning heading the total shares with its lines.
        if let Some(o) = self.span_group(t) {
            cands.push(plus((o..t).filter(|&r| self.participates(r)).collect()));
        }
        // The run of rows directly above ranked below the total (the parts
        // of the line it closes), at their top level or as leaves.
        let mut run = Vec::new();
        let level = self.level(t);
        for r in (self.first_data..t).rev() {
            if self.is_heading(r) {
                break;
            }
            if !self.participates(r) {
                continue;
            }
            if self.level(r) <= level || self.rank(r).is_some() {
                break;
            }
            run.push(r);
        }
        run.reverse();
        cands.push(plus(top_level(self, run.clone())));
        cands.push(plus(self.leaves(&run)));
        // The block directly above, back to the previous total or heading:
        // its top level, every row, or the rows with no parts of their own.
        let mut block = Vec::new();
        for r in (self.first_data..t).rev() {
            if self.is_heading(r) || (self.participates(r) && self.rank(r).is_some()) {
                break;
            }
            if self.participates(r) {
                block.push(r);
            }
        }
        block.reverse();
        cands.push(plus(top_level(self, block.clone())));
        cands.push(plus(block.clone()));
        cands.push(plus(self.leaves(&block)));
        // Back to the previous total of the same rank, counting a lower-rank
        // total in place of the block it sums.
        let (mut details, mut totals) = (Vec::new(), Vec::new());
        let mut r = t;
        while r > self.first_data {
            r -= 1;
            if !self.participates(r) {
                continue;
            }
            match self.rank(r) {
                Some(k) if k >= rank => break,
                Some(_) => {
                    totals.push(r);
                    while r > self.first_data
                        && !self.is_heading(r - 1)
                        && !(self.participates(r - 1) && self.rank(r - 1).is_some())
                    {
                        r -= 1;
                    }
                }
                None => details.push(r),
            }
        }
        for mut rows in [top_level(self, details.clone()), details] {
            rows.extend(&totals);
            rows.sort_unstable();
            cands.push(plus(rows));
        }
        // Every line above at the top level, totals left out (매출원가 =
        // 제품매출원가 + 상품매출원가 + 기타매출원가 around their own 계 rows).
        let lines: Vec<usize> =
            (self.first_data..t).filter(|&r| self.participates(r) && self.rank(r).is_none()).collect();
        cands.push(plus(top_level(self, lines)));
        // Totals of the same rank just above (부채총계 + 자본총계).
        let same: Vec<usize> =
            (self.first_data..t).rev().filter(|&r| self.participates(r) && self.rank(r) == Some(rank)).collect();
        for n in 2..=same.len() {
            let mut rows = same[..n].to_vec();
            rows.reverse();
            cands.push(plus(rows));
        }
        cands
    }

    /// Closing balance = opening balance + the movements between them.
    fn movement(&self, t: usize) -> Vec<Cand> {
        let Some(o) = (self.first_data..t).rev().find(|&r| self.participates(r) && self.is_opening(r)) else {
            return Vec::new();
        };
        let all: Vec<usize> = (o + 1..t).filter(|&r| self.participates(r)).collect();
        let mut cands = Vec::new();
        for between in [top_level(self, all.clone()), self.leaves(&all), all] {
            let mut plain: Cand = vec![(1, o)];
            plain.extend(between.iter().map(|&r| (1, r)));
            let mut signed: Cand = vec![(1, o)];
            signed.extend(between.iter().map(|&r| (self.label[r - 1].as_deref().map_or(1, decrease_sign), r)));
            cands.push(plain);
            cands.push(signed);
        }
        cands
    }

    /// The rows among `rows` that have no parts of their own.
    fn leaves(&self, rows: &[usize]) -> Vec<usize> {
        rows.iter().copied().filter(|&r| self.is_opening(r) || self.children(r).is_empty()).collect()
    }

    /// The first row of a row-spanning label cell that covers `t` from above.
    fn span_group(&self, t: usize) -> Option<usize> {
        (1..=self.table.ncols()).filter(|c| !self.figure_cols.contains(c)).find_map(|c| {
            let cell = self.table.cell_at_grid(t - 1, c - 1)?;
            (cell.rowspan > 1 && cell.row < t && cell.row >= self.first_data).then_some(cell.row)
        })
    }

    /// Signed chains of the sibling lines just above `t` (statement lines
    /// such as 매출총이익 = 매출액 − 매출원가).
    fn chains(&self, t: usize) -> Vec<Cand> {
        let level = self.level(t);
        let mut siblings = Vec::new();
        for r in (self.first_data..t).rev() {
            if !self.participates(r) {
                continue;
            }
            let l = self.level(r);
            if l < level {
                break;
            }
            if l == level {
                siblings.push(r);
            }
        }
        let mut cands = Vec::new();
        // The line above plus the detail lines under it (법인세비용차감전순이익
        // = 영업이익 + 기타수익 − 기타비용 + 금융수익 − 금융비용).
        if let Some(&p) = siblings.first() {
            let parts = self.children(p);
            if !parts.is_empty() {
                let mut signed: Cand = vec![(1, p)];
                signed.extend(parts.iter().map(|&r| (self.sign_word(r), r)));
                let mut plain: Cand = vec![(1, p)];
                plain.extend(parts.iter().map(|&r| (1, r)));
                cands.push(signed);
                cands.push(plain);
            }
        }
        for n in 2..=siblings.len().min(MAX_CHAIN) {
            let mut rows = siblings[..n].to_vec();
            rows.reverse();
            cands.push(rows.iter().map(|&r| (self.sign_word(r), r)).collect());
            cands.push(rows.iter().map(|&r| (1, r)).collect());
        }
        cands
    }

    fn vertical_check(&self, g: &[usize], t: usize, (rule, kind, axis): (Rule, Kind, &str), cand: &Cand) -> Check {
        let col = self.in_group(g, t).map_or(g[0], |(c, _)| c);
        let mut rows: Vec<(i8, usize, usize)> = cand.iter().map(|&(s, r)| (s, r, self.col_of(g, r))).collect();
        rows.sort_by_key(|&(_, r, _)| r);
        let mut terms: Vec<Term> = Vec::new();
        for (sign, r, c) in rows {
            if let Some(last) = terms.last_mut()
                && last.sign == sign
                && last.cols == (c, c)
                // Only across rows with nothing in this period: a pair's
                // detail figures beside a blank are parts, not zeros.
                && (last.rows.1 + 1..r).all(|between| g.iter().all(|&gc| self.at(between, gc) == V::Blank))
            {
                last.rows.1 = r;
                continue;
            }
            terms.push(Term { sign, table: self.table.index, rows: (r, r), cols: (c, c) });
        }
        self.check(rule, kind, axis, (t, col), terms)
    }

    /// The column a row's figure sits in within a group; for a blank, the
    /// column its level uses elsewhere (a pair's detail column by default).
    fn col_of(&self, g: &[usize], r: usize) -> usize {
        if let Some((c, _)) = self.in_group(g, r) {
            return c;
        }
        if g.len() == 2 && self.level(r).0 == 0 && self.groups.iter().any(|h| h.len() == 2 && self.num(h, r).is_some())
        {
            return g[1];
        }
        g[0]
    }

    fn check(&self, rule: Rule, kind: Kind, axis: &str, (row, col): (usize, usize), terms: Vec<Term>) -> Check {
        let t = self.table.index;
        let label = super::row_label(self.doc, self.table, row);
        Check {
            id: format!("{axis}:T{t}.R{row}.C{col}"),
            kind,
            origin: Origin::Auto,
            rule: Some(rule),
            target: CellRef {
                table: t,
                row,
                col,
                row_label: label,
                col_label: col_label(self.doc, self.table, col),
                section: None,
            },
            terms,
            tolerance: None,
            note: None,
        }
    }

    // -----------------------------------------------------------------------
    // horizontal: totals across a row

    // -----------------------------------------------------------------------
    // identities: 자산총계 = 부채와자본총계, or = 부채총계 + 자본총계

    /// Assert the balance identity wherever its lines meet in one table, down
    /// the rows or across the columns; the columns it explains are returned
    /// so they are not reported as totals with nothing to sum.
    fn identities(&self, out: &mut Proposals) -> Vec<usize> {
        let find = |keys: &[String], names: &[&str]| keys.iter().position(|k| names.contains(&k.as_str()));
        let relation = |keys: &[String]| -> Option<(usize, Vec<usize>)> {
            let assets = find(keys, &["자산총계"])?;
            if let Some(le) = find(keys, &["부채와자본총계", "부채및자본총계", "자본과부채총계"]) {
                return Some((assets, vec![le]));
            }
            Some((assets, vec![find(keys, &["부채총계"])?, find(keys, &["자본총계"])?]))
        };
        let rows: Vec<usize> = self.data_rows().filter(|&r| self.participates(r)).collect();
        let row_keys: Vec<String> =
            rows.iter().map(|&r| self.label[r - 1].as_deref().map_or_else(String::new, key)).collect();
        if let Some((a, terms)) = relation(&row_keys) {
            let cand: Cand = terms.iter().map(|&i| (1, rows[i])).collect();
            for g in self.emitted(rows[a], &cand) {
                out.checks.push(self.vertical_check(g, rows[a], (Rule::Identity, Kind::Identity, "i"), &cand));
            }
        }
        if self.groups.iter().any(|g| g.len() == 2) {
            return Vec::new();
        }
        let cols = &self.figure_cols;
        let col_keys: Vec<String> = cols
            .iter()
            .map(|&c| col_label(self.doc, self.table, c).map_or_else(String::new, |l| key(last_part(&l))))
            .collect();
        let Some((a, terms)) = relation(&col_keys) else { return Vec::new() };
        let cand: Cand = terms.iter().map(|&i| (1, cols[i])).collect();
        for r in self.emitted_rows(cols[a], &cand) {
            out.checks.push(self.row_check((Rule::Identity, Kind::Identity, "i"), &cand, r, cols[a]));
        }
        let mut explained = vec![cols[a]];
        explained.extend(terms.iter().map(|&i| cols[i]));
        explained
    }

    fn horizontal(&self, out: &mut Proposals, explained: &[usize]) {
        if self.groups.iter().any(|g| g.len() == 2) {
            return; // a statement layout; its columns are periods, not parts
        }
        let cols = &self.figure_cols;
        let labels: Vec<String> =
            cols.iter().map(|&c| col_label(self.doc, self.table, c).unwrap_or_default()).collect();
        let bottoms: Vec<Option<String>> = cols.iter().map(|&c| self.bottom_header(c)).collect();
        let is_total = |j: usize| total_rank(last_part(&labels[j])).is_some();
        for (i, &target) in cols.iter().enumerate() {
            let rank = total_rank(last_part(&labels[i]));
            let closing = balance_word(&labels[i], "기말");
            // Under a two-level header, a total sums only its own block.
            let same_block = |j: usize| !labels[i].contains(" / ") || first_part(&labels[j]) == first_part(&labels[i]);
            let left: Vec<usize> = (0..i).rev().take_while(|&j| !is_total(j) && same_block(j)).collect();
            let plus = |js: &[usize]| -> Cand { js.iter().rev().map(|&j| (1, cols[j])).collect() };
            let mut cands: Vec<(Rule, Kind, Cand, bool)> = Vec::new();
            if closing && let Some(o) = (0..i).rev().find(|&j| balance_word(&labels[j], "기초")) {
                let plain: Cand = (o..i).map(|j| (1, cols[j])).collect();
                let signed: Cand =
                    (o..i).map(|j| (if j == o { 1 } else { decrease_sign(&labels[j]) }, cols[j])).collect();
                cands.push((Rule::ColumnMovement, Kind::Movement, plain, false));
                cands.push((Rule::ColumnMovement, Kind::Movement, signed, false));
            }
            if let Some(rank) = rank {
                // Back to a total of the same rank, a lower-rank total (소계)
                // counting in place of the columns under its heading.
                let mut walk: Vec<usize> = Vec::new();
                let mut j = i;
                while j > 0 {
                    j -= 1;
                    match total_rank(last_part(&labels[j])) {
                        Some(k) if k >= rank => break,
                        Some(_) => {
                            walk.push(j);
                            let head = first_part(&labels[j]).to_owned();
                            while j > 0 && labels[j - 1].contains(" / ") && first_part(&labels[j - 1]) == head {
                                j -= 1;
                            }
                        }
                        None => walk.push(j),
                    }
                }
                cands.push((Rule::ColumnTotal, Kind::Horizontal, plus(&walk), false));
                let right: Vec<usize> = (i + 1..cols.len()).take_while(|&j| !is_total(j) && same_block(j)).collect();
                cands.push((Rule::ColumnTotal, Kind::Horizontal, plus(&left), false));
                cands.push((Rule::ColumnTotal, Kind::Horizontal, right.iter().map(|&j| (1, cols[j])).collect(), false));
                // The columns just left of it (지배기업지분 + 비지배지분 = 총계).
                for n in 2..left.len() {
                    cands.push((Rule::ColumnTotal, Kind::Horizontal, plus(&left[..n]), false));
                }
                // Columns under the same sub-heading (합계/당기 = Σ 당기).
                if let Some(b) = &bottoms[i] {
                    let same: Cand = (0..cols.len())
                        .filter(|&j| j != i && !is_total(j) && bottoms[j].as_ref() == Some(b))
                        .map(|j| (1, cols[j]))
                        .collect();
                    if same.len() >= 2 {
                        cands.push((Rule::ColumnTotal, Kind::Horizontal, same, false));
                    }
                }
            }
            // Without a label, a column that sums the columns left of it in
            // most rows is a total too (자본 합계 beside its components).
            if left.len() >= 2 {
                let signed: Cand = left.iter().rev().map(|&j| (decrease_sign(&labels[j]), cols[j])).collect();
                cands.push((Rule::ColumnTotal, Kind::Horizontal, plus(&left), true));
                cands.push((Rule::ColumnTotal, Kind::Horizontal, signed, true));
            }
            cands.retain(|(_, _, c, _)| !c.is_empty());
            let mut seen: Vec<Cand> = Vec::new();
            cands.retain(|(_, _, c, _)| {
                let new = !seen.contains(c);
                seen.push(c.clone());
                new
            });
            let needed = (rank.is_some() || closing) && !explained.contains(&target);
            if cands.is_empty() {
                if needed {
                    let rows = self.data_rows().filter(|&r| matches!(self.at(r, target), V::Num(_)));
                    out.uncovered.extend(rows.map(|r| (Kind::Horizontal, self.table.index, r, target)));
                }
                continue;
            }
            self.choose_columns(target, &cands, needed, out);
        }
    }

    /// A check across row `r`: column `target` = the signed columns in `cand`.
    fn row_check(&self, (rule, kind, axis): (Rule, Kind, &str), cand: &Cand, r: usize, target: usize) -> Check {
        let mut cols: Vec<(i8, usize)> = cand.clone();
        cols.sort_by_key(|&(_, c)| c);
        let mut terms: Vec<Term> = Vec::new();
        for (sign, c) in cols {
            if let Some(last) = terms.last_mut()
                && last.sign == sign
                && (last.cols.1 + 1..c).all(|between| self.at(r, between) == V::Blank)
            {
                last.cols.1 = c;
                continue;
            }
            terms.push(Term { sign, table: self.table.index, rows: (r, r), cols: (c, c) });
        }
        self.check(rule, kind, axis, (r, target), terms)
    }

    /// The header text directly above the first data row in column `c`.
    fn bottom_header(&self, c: usize) -> Option<String> {
        let r = self.first_data.checked_sub(1).filter(|&r| r >= 1)?;
        let cell = self.table.cell_at_grid(r - 1, c - 1)?;
        if cell.row != r {
            return None;
        }
        let text = compact(&node_text_outside_tables(&self.doc.tree, cell.node));
        (!text.is_empty()).then_some(text)
    }

    fn row_sum(&self, r: usize, cand: &Cand) -> Option<f64> {
        let mut total = 0.0;
        for &(sign, c) in cand {
            match self.at(r, c) {
                V::Num(x) => total += f64::from(sign) * x,
                V::Text | V::Missing => return None,
                V::Blank | V::Covered => {}
            }
        }
        Some(total)
    }

    /// The rows where `cand` can make column `target` a total (as
    /// [`Grid::applicable`] for columns).
    fn applicable_rows(&self, target: usize, cand: &Cand) -> Vec<usize> {
        self.data_rows()
            .filter(|&r| {
                matches!(self.at(r, target), V::Num(_))
                    && self.row_sum(r, cand).is_some_and(|s| !self.dash[r - 1][target - 1] || equal(s, 0.0))
            })
            .collect()
    }

    /// The rows a chosen structure is stated for (as [`Grid::emitted`] for columns).
    fn emitted_rows(&self, target: usize, cand: &Cand) -> Vec<usize> {
        self.data_rows()
            .filter(|&r| {
                let dash = self.dash[r - 1][target - 1];
                matches!(self.at(r, target), V::Num(_))
                    && self.row_sum(r, cand).map_or(!dash, |s| !dash || equal(s, 0.0))
            })
            .collect()
    }

    fn row_miss(&self, target: usize, cand: &Cand) -> f64 {
        self.applicable_rows(target, cand)
            .iter()
            .map(|&r| match (self.at(r, target), self.row_sum(r, cand)) {
                (V::Num(a), Some(e)) => (a - e).abs(),
                _ => f64::INFINITY,
            })
            .sum()
    }

    fn choose_columns(&self, target: usize, cands: &[(Rule, Kind, Cand, bool)], needed: bool, out: &mut Proposals) {
        let nonzero = |r: usize, cand: &Cand| {
            cand.iter().filter(|&&(_, c)| matches!(self.at(r, c), V::Num(x) if x != 0.0)).count()
        };
        let mut best: Option<((usize, usize), usize)> = None;
        for (i, (_, _, cand, strict)) in cands.iter().enumerate() {
            let rows = self.applicable_rows(target, cand);
            let holding: Vec<usize> = rows
                .iter()
                .copied()
                .filter(|&r| matches!((self.at(r, target), self.row_sum(r, cand)), (V::Num(a), Some(e)) if equal(a, e)))
                .collect();
            let nontrivial =
                holding.iter().filter(|&&r| self.at(r, target) != V::Num(0.0) || nonzero(r, cand) > 0).count();
            if *strict {
                // Rows where the total itself is 0 prove nothing about a column
                // found without a label (parts that net to zero beside a dash).
                let substantive =
                    holding.iter().filter(|&&r| nonzero(r, cand) >= 2 && self.at(r, target) != V::Num(0.0)).count();
                // More than half its rows, so a coincidence in some rows
                // cannot assert a total that the others then fail.
                if holding.len() < 2 || holding.len() * 2 <= rows.len() || substantive < 2 {
                    continue;
                }
            }
            if nontrivial == 0 && (holding.is_empty() || holding.len() < rows.len()) {
                continue;
            }
            let score = (nontrivial, holding.len());
            if best.is_none_or(|(s, _)| score > s) {
                best = Some((score, i));
            }
        }
        let emit = |rule: Rule, kind: Kind, cand: &Cand, r: usize| self.row_check((rule, kind, "h"), cand, r, target);
        if let Some((_, i)) = best {
            let (rule, kind, cand, _) = &cands[i];
            out.checks.extend(self.emitted_rows(target, cand).into_iter().map(|r| emit(*rule, *kind, cand, r)));
            return;
        }
        if !needed {
            return;
        }
        let closest = cands
            .iter()
            .filter(|(_, _, cand, strict)| !strict && !self.applicable_rows(target, cand).is_empty())
            .min_by(|a, b| self.row_miss(target, &a.2).total_cmp(&self.row_miss(target, &b.2)));
        if let Some((rule, kind, cand, _)) = closest {
            out.unresolved.extend(self.emitted_rows(target, cand).into_iter().map(|r| emit(*rule, *kind, cand, r)));
        }
    }
}

/// A label reduced to its words: no numbering, spaces, or trailing
/// parenthetical (`Ⅲ. 자 산 총 계` → `자산총계`).
pub(super) fn key(label: &str) -> String {
    bare(label).trim_start_matches(|c: char| !is_hangul(c)).to_owned()
}

/// The rows at the highest level among `rows` (the others are their parts).
fn top_level(grid: &Grid, rows: Vec<usize>) -> Vec<usize> {
    let Some(min) = rows.iter().map(|&r| grid.level(r)).min() else { return rows };
    rows.into_iter().filter(|&r| grid.level(r) == min).collect()
}

/// Whether a column label names figures that add up. Rates (`이자율`,
/// `지분율`), prices, averages, issue numbers, dates, and note references do
/// not.
fn additive(label: &str) -> bool {
    let w = bare(last_part(label));
    let w = compact(&w);
    let never = ["주석", "가격", "단가", "평균", "회차", "일자", "기간", "주가"];
    !(never.iter().any(|s| w.contains(s)) || w.ends_with(['율', '률', '일']))
}

/// A column labelled as an opening or closing balance (`기초`, `기말 장부금액`),
/// not as a period (`당기말`, `3분기말`, `제 35 기말`).
fn balance_word(label: &str, word: &str) -> bool {
    compact(last_part(label)).starts_with(word)
}

/// Whether an opening row continues the closing row above it: the day after
/// it when both carry a date (`2024.12.31` → `2025.01.01`), else unless the
/// closing ends an interim period (`전반기말`), whose next year opens elsewhere.
fn continues(closing: &str, opening: &str) -> bool {
    match (date_in(closing), date_in(opening)) {
        (Some(c), Some(o)) => next_day(c) == o,
        _ => !(closing.contains("반기") || closing.contains("분기")),
    }
}

fn date_in(text: &str) -> Option<(u32, u32, u32)> {
    let digits =
        |s: &str, len: std::ops::RangeInclusive<usize>| len.contains(&s.len()) && s.bytes().all(|b| b.is_ascii_digit());
    let chars: Vec<char> = compact(text).chars().collect();
    (0..chars.len()).find_map(|start| {
        let rest: String = chars[start..].iter().collect();
        let mut parts = rest.splitn(3, ['.', '-', '/']);
        let (y, m, d) = (parts.next()?, parts.next()?, parts.next()?);
        let d: String = d.chars().take_while(char::is_ascii_digit).collect();
        if !(digits(y, 4..=4) && digits(m, 1..=2) && digits(&d, 1..=2)) {
            return None;
        }
        Some((y.parse().ok()?, m.parse().ok()?, d.parse().ok()?))
    })
}

fn next_day((y, m, d): (u32, u32, u32)) -> (u32, u32, u32) {
    let leap = y % 4 == 0 && (y % 100 != 0 || y % 400 == 0);
    let last = match m {
        2 if leap => 29,
        2 => 28,
        4 | 6 | 9 | 11 => 30,
        _ => 31,
    };
    match (d >= last, m == 12) {
        (false, _) => (y, m, d + 1),
        (true, false) => (y, m + 1, 1),
        (true, true) => (y + 1, 1, 1),
    }
}

fn equal(a: f64, e: f64) -> bool {
    (a - e).abs() <= super::rounding_slack(a.abs().max(e.abs()))
}

/// `당기 / 합계` → `당기`; a label without parts is its own first part.
fn first_part(label: &str) -> &str {
    label.split(" / ").next().unwrap_or(label)
}

fn last_part(label: &str) -> &str {
    label.rsplit(" / ").next().unwrap_or(label)
}

/// The label without its numbering, spaces, or a trailing parenthetical.
fn bare(label: &str) -> String {
    let mut s = compact(label);
    while s.ends_with(')')
        && let Some(open) = s.rfind('(')
        && open > 0
    {
        s.truncate(open);
    }
    s
}

/// 1 for 소계, 2 for 합계 or a separate 계 (`유동자산 계`), 3 for 총계; None
/// for other labels.
fn total_rank(word: &str) -> Option<u8> {
    let w = bare(word);
    let w = w.trim_start_matches(|c: char| !is_hangul(c));
    if w.ends_with("소계") {
        Some(1)
    } else if w.ends_with("총계") {
        Some(3)
    } else if w.ends_with("합계") || w == "계" || collapse(word).ends_with(" 계") {
        Some(2)
    } else {
        None
    }
}

fn is_hangul(c: char) -> bool {
    ('가'..='힣').contains(&c)
}

/// The syllables Korean lists count with: 가, 나, 다, ...
fn is_ordinal(c: char) -> bool {
    "가나다라마바사아자차카타파하".contains(c)
}

/// −1 for a cost or loss line (매출원가, 판매비와관리비, 법인세비용).
fn expense_sign(label: &str) -> i8 {
    let w = bare(&collapse(label));
    let costs = ["비용", "원가", "손실", "관리비", "차손"];
    if costs.iter().any(|s| w.ends_with(s)) { -1 } else { 1 }
}

/// −1 for a decrease in a movement (감소, 처분, 상각, 손상, 배당).
fn decrease_sign(label: &str) -> i8 {
    let w = compact(label);
    let decreases = ["감소", "처분", "상각", "손상", "배당", "상환", "지급", "사용"];
    if decreases.iter().any(|s| w.contains(s)) { -1 } else { 1 }
}

/// A label's numbering rank: Ⅰ. > 1. > 가. > (1) > ① > (가) > unnumbered,
/// where an unnumbered, unindented label (a total line) ranks first and
/// indentation ranks the rest.
fn numbering(raw: &str) -> u16 {
    let indent: u16 = raw.chars().take_while(|c| c.is_whitespace()).map(|c| if c == '\u{3000}' { 2 } else { 1 }).sum();
    let s = raw.trim_start();
    let mut chars = s.chars();
    let Some(first) = chars.next() else { return 0 };
    let second = chars.next();
    let digits: String = s.chars().take_while(char::is_ascii_digit).collect();
    let after_digits = s[digits.len()..].chars().next();
    let after_after = s[digits.len()..].chars().nth(1);
    if ('\u{2160}'..='\u{217f}').contains(&first) {
        return 1;
    }
    if let Some(dot) = s.find('.')
        && dot > 0
        && s[..dot].chars().all(|c| matches!(c, 'I' | 'V' | 'X' | 'L'))
    {
        return 1;
    }
    if !digits.is_empty()
        && (after_digits == Some(')')
            || (after_digits == Some('.') && !after_after.is_some_and(|c| c.is_ascii_digit())))
    {
        return 2;
    }
    if is_ordinal(first) && second == Some('.') {
        return 3;
    }
    if first == '(' && second.is_some_and(|c| c.is_ascii_digit()) {
        return 4;
    }
    if ('\u{2460}'..='\u{2473}').contains(&first) {
        return 5;
    }
    // `(가)`, but not `(주)`, a company marker.
    if first == '(' && second.is_some_and(is_ordinal) && s.chars().nth(2) == Some(')') {
        return 6;
    }
    // Bullets (`- 현금`) rank by indentation, like unnumbered lines.
    if indent == 0 { 0 } else { 8 + indent.min(60) }
}

#[cfg(test)]
mod tests {
    use std::fmt::Write as _;

    use super::*;
    use crate::foot::testing::{doc, table};
    use crate::foot::{Report, Status, foot};

    /// Each check as `id rule status terms`, for compact assertions.
    fn lines(report: &Report) -> Vec<String> {
        report
            .checks
            .iter()
            .map(|c| {
                let mut s = format!("{} {:?} {:?}", c.id, c.rule.unwrap(), c.status);
                for t in &c.terms {
                    let _ = write!(
                        s,
                        " {}R{}-{}C{}-{}",
                        if t.sign > 0 { '+' } else { '-' },
                        t.rows.0,
                        t.rows.1,
                        t.cols.0,
                        t.cols.1
                    );
                }
                s
            })
            .collect()
    }

    #[test]
    fn statement_subtotals_sum_their_detail_column_and_the_balance_identity_holds() {
        let d = doc(&table(
            &["과 목", "당기|2", "전기|2"],
            &[
                &["Ⅰ.유동자산", "", "300", "", "250"],
                &["   현금", "100", "", "100", ""],
                &["   매출채권", "200", "", "150", ""],
                &["Ⅱ.비유동자산", "", "500", "", "400"],
                &["   유형자산", "500", "", "400", ""],
                &["자산총계", "", "800", "", "650"],
                &["Ⅰ.유동부채", "", "300", "", "300"],
                &["   차입금", "300", "", "300", ""],
                &["부채총계", "", "300", "", "300"],
                &["Ⅰ.자본금", "", "500", "", "350"],
                &["   보통주자본금", "500", "", "350", ""],
                &["자본총계", "", "500", "", "350"],
                &["부채와자본총계", "", "800", "", "660"],
            ],
        ));
        let r = foot(&d);
        assert_eq!(
            lines(&r),
            [
                "v:T1.R2.C3 Hierarchy Pass +R3-4C2-2",
                "v:T1.R2.C5 Hierarchy Pass +R3-4C4-4",
                "v:T1.R5.C3 Hierarchy Pass +R6-6C2-2",
                "v:T1.R5.C5 Hierarchy Pass +R6-6C4-4",
                "v:T1.R7.C3 LabelTotal Pass +R2-2C3-3 +R5-5C3-3",
                "v:T1.R7.C5 LabelTotal Pass +R2-2C5-5 +R5-5C5-5",
                "v:T1.R8.C3 Hierarchy Pass +R9-9C2-2",
                "v:T1.R8.C5 Hierarchy Pass +R9-9C4-4",
                "v:T1.R10.C3 LabelTotal Pass +R8-8C3-3",
                "v:T1.R10.C5 LabelTotal Pass +R8-8C5-5",
                "v:T1.R11.C3 Hierarchy Pass +R12-12C2-2",
                "v:T1.R11.C5 Hierarchy Pass +R12-12C4-4",
                "v:T1.R13.C3 LabelTotal Pass +R11-11C3-3",
                "v:T1.R13.C5 LabelTotal Pass +R11-11C5-5",
                // The prior period's 660 is 10 more than its parts: a finding.
                "v:T1.R14.C3 LabelTotal Pass +R10-10C3-3 +R13-13C3-3",
                "v:T1.R14.C5 LabelTotal Fail +R10-10C5-5 +R13-13C5-5",
                "i:T1.R7.C3 Identity Pass +R14-14C3-3",
                "i:T1.R7.C5 Identity Fail +R14-14C5-5",
            ]
        );
        assert_eq!((r.unresolved.len(), r.uncovered.len()), (0, 0));
        assert_eq!(r.checks[15].difference, Some(10.0));
        assert_eq!(r.checks[0].target.row_label.as_deref(), Some("Ⅰ.유동자산"));
    }

    #[test]
    fn income_statement_lines_chain_with_signs() {
        let d = doc(&table(
            &["과 목", "주 석", "당기", "전기"],
            &[
                &["Ⅰ.매출액", "4", "1,000", "900"],
                &["Ⅱ.매출원가", "5", "600", "500"],
                &["Ⅲ.매출총이익", "", "400", "400"],
                &["Ⅳ.판매비와관리비", "6", "150", "100"],
                &["Ⅴ.영업이익", "", "250", "300"],
            ],
        ));
        let r = foot(&d);
        assert_eq!(
            lines(&r),
            [
                "v:T1.R4.C3 SignedSum Pass +R2-2C3-3 -R3-3C3-3",
                "v:T1.R4.C4 SignedSum Pass +R2-2C4-4 -R3-3C4-4",
                "v:T1.R6.C3 SignedSum Pass +R4-4C3-3 -R5-5C3-3",
                "v:T1.R6.C4 SignedSum Pass +R4-4C4-4 -R5-5C4-4",
            ],
            "the note column is never summed, and line items are not totals"
        );
        assert_eq!((r.unresolved.len(), r.uncovered.len()), (0, 0));
    }

    #[test]
    fn note_totals_skip_rates_and_count_subtotals_in_place_of_their_block() {
        let d = doc(&table(
            &["구 분", "지분율", "당기", "전기"],
            &[
                &["&lt;유동&gt;|4"],
                &["갑", "10.5", "100", "80"],
                &["을", "20.0", "200", "120"],
                &["소 계", "30.5", "300", "200"],
                &["&lt;비유동&gt;|4"],
                &["병", "1.0", "50", "40"],
                &["합 계", "31.5", "350", "240"],
            ],
        ));
        assert_eq!(
            lines(&foot(&d)),
            [
                "v:T1.R5.C3 LabelTotal Pass +R3-4C3-3",
                "v:T1.R5.C4 LabelTotal Pass +R3-4C4-4",
                "v:T1.R8.C3 LabelTotal Pass +R5-5C3-3 +R7-7C3-3",
                "v:T1.R8.C4 LabelTotal Pass +R5-5C4-4 +R7-7C4-4",
            ],
            "a rate column (지분율) is never summed"
        );
    }

    #[test]
    fn movements_close_down_the_rows_and_across_the_columns() {
        let d = doc(&format!(
            "{}{}",
            table(
                &["구 분", "당기", "전기"],
                &[
                    &["기초", "1,000", "800"],
                    &["  증가", "300", "400"],
                    &["  감소", "(100)", "(200)"],
                    &["기말", "1,200", "1,000"]
                ],
            ),
            table(
                &["구 분", "기초", "취득", "상각", "기말"],
                &[&["건물", "1,000", "200", "50", "1,150"], &["비품", "300", "-", "100", "200"]],
            ),
        ));
        assert_eq!(
            lines(&foot(&d)),
            [
                "v:T1.R5.C2 Movement Pass +R2-4C2-2",
                "v:T1.R5.C3 Movement Pass +R2-4C3-3",
                "h:T2.R2.C5 ColumnMovement Pass +R2-2C2-3 -R2-2C4-4",
                "h:T2.R3.C5 ColumnMovement Pass +R3-3C2-3 -R3-3C4-4",
                // The current period opens with the prior period's closing.
                "x:T1.R2.C2 PriorClosing Pass +R5-5C3-3",
            ],
            "a decrease shown without parentheses is subtracted by its label"
        );
    }

    #[test]
    fn a_total_nothing_explains_is_unresolved_and_one_with_nothing_to_sum_is_uncovered() {
        let d = doc(&table(&["구 분", "당기"], &[&["합 계", "100"], &["갑", "10"], &["을", "20"], &["합 계", "50"]]));
        let r = foot(&d);
        assert!(r.checks.is_empty());
        let un: Vec<(String, Option<f64>)> = r.unresolved.iter().map(|c| (c.id.clone(), c.difference)).collect();
        assert_eq!(un, [("v:T1.R5.C2".to_owned(), Some(20.0))], "the closest candidate is the rows above");
        assert_eq!(r.unresolved[0].status, Status::Fail);
        let uncovered: Vec<(usize, usize)> = r.uncovered.iter().map(|u| (u.cell.row, u.cell.col)).collect();
        assert_eq!(uncovered, [(2, 2)]);
        assert_eq!((r.counts.unresolved, r.counts.uncovered), (1, 1));
    }

    #[test]
    fn the_balance_identity_holds_across_columns() {
        let d = doc(&table(
            &["회사", "자산총계", "부채총계", "자본총계", "매출"],
            &[&["갑", "100", "60", "40", "500"], &["을", "200", "50", "140", "10"]],
        ));
        let r = foot(&d);
        assert_eq!(lines(&r), ["i:T1.R2.C2 Identity Pass +R2-2C3-4", "i:T1.R3.C2 Identity Fail +R3-3C3-4"]);
        assert_eq!((r.unresolved.len(), r.uncovered.len()), (0, 0), "the identity explains its columns");
    }

    #[test]
    fn a_coincidence_on_zeros_proves_nothing() {
        // 기타 = 가 + 나 only because everything is zero; nothing is proposed.
        let d = doc(&table(&["구 분", "당기", "전기"], &[&["가", "-", "-"], &["나", "-", "-"], &["기타", "-", "-"]]));
        let r = foot(&d);
        assert!(r.checks.is_empty() && r.unresolved.is_empty() && r.uncovered.is_empty());
    }

    #[test]
    fn a_lone_dash_total_over_figures_is_not_a_total() {
        let d = doc(&table(
            &["구 분", "당기", "비고"],
            &[&["갑", "100", "1"], &["을", "200", "2"], &["합 계", "300", "-"]],
        ));
        assert_eq!(lines(&foot(&d)), ["v:T1.R4.C2 LabelTotal Pass +R2-3C2-2"]);
    }

    #[test]
    fn a_lone_dash_over_parts_that_net_to_zero_is_a_total_of_zero() {
        // 회원권 moves out entirely: its closing is a dash, and the row adds up.
        let d = doc(&table(
            &["구 분", "기초", "취득", "대체", "합계"],
            &[
                &["회원권", "100", "-", "(100)", "-"],
                &["소프트웨어", "50", "10", "100", "160"],
                &["합 계", "150", "10", "-", "160"],
            ],
        ));
        let r = foot(&d);
        let row2: Vec<&str> = r.checks.iter().filter(|c| c.target.row == 2).map(|c| c.id.as_str()).collect();
        assert_eq!(row2, ["h:T1.R2.C5"]);
        assert!(r.checks.iter().all(|c| c.status == Status::Pass));
    }

    #[test]
    fn rows_that_net_to_zero_do_not_make_a_column_a_total() {
        // 손상차손누계액 equals 취득원가 + 상각누계액 only where all three are spent.
        let d = doc(&table(
            &["구 분", "취득원가", "상각누계액", "손상차손누계액", "장부금액"],
            &[
                &["가", "100", "(60)", "-", "40"],
                &["나", "50", "(20)", "(5)", "25"],
                &["다", "10", "(10)", "-", "-"],
                &["라", "8", "(8)", "-", "-"],
                &["합 계", "168", "(98)", "(5)", "65"],
            ],
        ));
        let r = foot(&d);
        assert!(
            r.checks.iter().all(|c| !(c.id.starts_with("h:") && c.target.col == 4)),
            "{:?}",
            r.checks.iter().map(|c| &c.id).collect::<Vec<_>>()
        );
        assert!(r.checks.iter().all(|c| c.status == Status::Pass));
    }

    #[test]
    fn a_rate_row_does_not_take_its_column_out_of_the_sums() {
        let d = doc(&table(
            &["구 분", "당 기", "전 기"],
            &[
                &["조정사항", "", ""],
                &["  가", "10", "20"],
                &["  나", "(3)", "5"],
                &["소 계", "7", "25"],
                &["평균유효세율", "1.3%", "14.3%"],
            ],
        ));
        let ids: Vec<String> = foot(&d).checks.iter().map(|c| format!("{} {:?}", c.id, c.status)).collect();
        assert_eq!(ids, ["v:T1.R5.C2 Pass", "v:T1.R5.C3 Pass"]);
    }

    #[test]
    fn a_position_no_cell_covers_is_never_summed() {
        // The 병 row is one cell short: its C3 is not a blank the evaluator can read.
        let d = doc(&table(
            &["구 분", "당기", "전기"],
            &[&["갑", "100", "80"], &["을", "200", "120"], &["병", "50"], &["합 계", "350", "200"]],
        ));
        let r = foot(&d);
        // The prior period is stated too, and needs judgment rather than vanishing.
        assert_eq!(
            lines(&r),
            ["v:T1.R5.C2 LabelTotal Pass +R2-4C2-2", "v:T1.R5.C3 LabelTotal NeedsJudgment +R2-4C3-3"]
        );
        assert_eq!(r.checks[1].reason.as_deref(), Some("T1 R4C3 is not covered by any cell"));
    }

    #[test]
    fn a_period_with_text_in_its_parts_needs_judgment_instead_of_vanishing() {
        let d = doc(&table(
            &["구 분", "당기", "전기"],
            &[&["갑", "100", "80"], &["을", "200", "주석 5"], &["합 계", "300", "200"]],
        ));
        let r = foot(&d);
        let shown: Vec<(String, Status)> = r.checks.iter().map(|c| (c.id.clone(), c.status)).collect();
        assert_eq!(shown, [("v:T1.R4.C2".to_owned(), Status::Pass), ("v:T1.R4.C3".to_owned(), Status::NeedsJudgment)]);
    }

    #[test]
    fn a_one_unit_difference_in_large_amounts_fails() {
        // At 10^12 a relative slack of 10^-9 would have absorbed 500.
        let d = doc(&table(
            &["구 분", "당기"],
            &[&["갑", "1,000,000,000,000"], &["을", "500"], &["병", "1,000"], &["합 계", "1,000,000,001,499"]],
        ));
        let r = foot(&d);
        let all: Vec<(Status, Option<f64>)> =
            r.checks.iter().chain(&r.unresolved).map(|c| (c.status, c.difference)).collect();
        assert_eq!(all, [(Status::Fail, Some(-1.0))]);
    }

    #[test]
    fn numbering_ranks() {
        assert_eq!(numbering("Ⅰ.유동자산"), 1);
        assert_eq!(numbering("XI.법인세비용"), 1);
        assert_eq!(numbering("　1. 영업에서"), 2);
        assert_eq!(numbering("　 가.기타"), 3);
        assert_eq!(numbering("(1) 기타"), 4);
        assert_eq!(numbering("(나) 기타"), 6);
        assert_eq!(numbering("(주)가나다"), 0);
        assert_eq!(numbering("- 현금"), 0);
        assert_eq!(numbering("   - 현금"), 11);
        assert_eq!(numbering("2024.01.01 (전기초)"), 0);
        assert_eq!(numbering("자산총계"), 0);
        assert_eq!(numbering("   현금"), 11);
    }

    #[test]
    fn openings_continue_year_end_closings() {
        assert!(continues("2024.12.31 (전기말)", "2025.01.01 (당기초)"));
        assert!(!continues("2025.06.30 (전반기말)", "2026.01.01 (당기초)"));
        assert!(continues("전기말", "당기초"));
        assert!(!continues("전반기말", "당기초"));
        assert_eq!(next_day((2024, 2, 28)), (2024, 2, 29));
    }

    #[test]
    fn total_labels() {
        assert_eq!(total_rank("소계"), Some(1));
        assert_eq!(total_rank("합계"), Some(2));
        assert_eq!(total_rank("계"), Some(2));
        assert_eq!(total_rank("자산총계"), Some(3));
        assert_eq!(total_rank("부채와자본총계"), Some(3));
        assert_eq!(total_rank("합계(주석5)"), Some(2));
        assert_eq!(total_rank("관계기업"), None);
        assert_eq!(total_rank("회계"), None);
        assert_eq!(expense_sign("Ⅱ.매출원가"), -1);
        assert_eq!(expense_sign("Ⅹ.법인세비용차감전순이익"), 1);
        assert_eq!(expense_sign("XI.법인세비용(수익)"), -1);
    }
}
