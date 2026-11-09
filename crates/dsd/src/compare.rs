//! Content-aligned comparison of two documents that may differ in form and
//! structure: a draft against the filed version, one quarter against the next.
//!
//! `diff` pairs blocks by position and suits an edit of one file. `compare`
//! instead pairs tables and paragraphs by content, so inserted, deleted, and
//! reordered tables stay aligned; inside a table pair it matches rows by their
//! account label (footnote markers and company-name forms ignored), detects
//! rows that moved, and matches columns by their headers when the column
//! counts differ. Numbers are compared as numbers and get a delta.

use std::collections::{HashMap, HashSet};
use std::fmt::Write as _;

use serde::Serialize;

use crate::difflib::{Matcher, Tag};
use crate::doc::{BLOCK_TEXT_TAGS, Doc, Table, is_scaffold};
use crate::text::{collapse, compact, header_info, node_text, node_text_outside_tables as own_text, prefix};
use crate::xml::NodeId;

// ---------------------------------------------------------------------------
// units

#[derive(Debug, Clone)]
struct Unit {
    /// `Some(index)` for a table, `None` for a text block.
    table: Option<usize>,
    node: NodeId,
    tag: String,
    text: String,
    line: usize,
    section: String,
}

/// `Some((number, title))` when a line opens a numbered note: "12. 종속기업투자".
/// A heading is short and is not a sentence; "3.1." is a subsection.
fn note_heading(line: &str) -> Option<(u32, String)> {
    let first = line.trim_start_matches(|c: char| c.is_whitespace());
    let digits: String = first.chars().take_while(char::is_ascii_digit).collect();
    if digits.is_empty() || digits.len() > 2 {
        return None;
    }
    let rest = first[digits.len()..].strip_prefix('.')?;
    if rest.starts_with(|c: char| c.is_ascii_digit()) {
        return None;
    }
    let rest = rest.trim();
    if rest.chars().count() > 40 || rest.ends_with("다.") || rest.ends_with('.') {
        return None;
    }
    let title: String = rest.chars().take_while(|&c| c != '(').collect::<String>().trim().to_owned();
    if title.is_empty() {
        return None;
    }
    Some((digits.parse().ok()?, title))
}

/// Note headings anywhere in a paragraph, with the 0-based line each starts.
fn note_headings(text: &str) -> Vec<(usize, u32, String)> {
    text.lines().enumerate().filter_map(|(i, l)| note_heading(l).map(|(n, t)| (i, n, t))).collect()
}

/// Notes run upward; a heading far ahead of the current note is a numbered list item.
fn next_note(current: Option<u32>, n: u32) -> bool {
    current.is_none_or(|c| n > c && n <= c + 5)
}

fn units(doc: &Doc) -> Vec<Unit> {
    struct Walk<'a> {
        doc: &'a Doc,
        region: String,
        note: Option<(u32, String)>,
        out: Vec<Unit>,
    }
    impl Walk<'_> {
        fn section(&self) -> String {
            match &self.note {
                Some((n, t)) => format!("{n}. {t}"),
                None => self.region.clone(),
            }
        }
        /// Tables inside `node` (in a cell or a paragraph) follow it as units
        /// of their own; the enclosing text shows only a placeholder for them.
        fn nested_tables(&mut self, node: NodeId) {
            let doc = self.doc;
            for n in doc.tree.iter_tag(node, "TABLE").into_iter().filter(|&n| n != node) {
                if let Some(t) = doc.table_for_node(n) {
                    let section = self.section();
                    self.out.push(Unit {
                        table: Some(t.index),
                        node: n,
                        tag: "TABLE".into(),
                        text: String::new(),
                        line: doc.line(doc.tree.node(n).start),
                        section,
                    });
                }
            }
        }
        fn walk(&mut self, node: NodeId) {
            let doc = self.doc;
            let tree = &doc.tree;
            for &child in tree.children(node) {
                let tag = tree.tag(child);
                if is_scaffold(tag) || tag == "DOCUMENT-HEADER" {
                    continue;
                }
                let line = doc.line(tree.node(child).start);
                if let Some(t) = doc.table_for_node(child) {
                    let section = self.section();
                    self.out.push(Unit {
                        table: Some(t.index),
                        node: child,
                        tag: tag.into(),
                        text: String::new(),
                        line,
                        section,
                    });
                    self.nested_tables(child);
                } else if BLOCK_TEXT_TAGS.contains(&tag) {
                    let text = own_text(tree, child);
                    if compact(&text).is_empty() {
                        continue;
                    }
                    if compact(&text) == crate::text::NESTED_TABLE_MARK {
                        // A wrapper around a table only: its tables are the content.
                        self.nested_tables(child);
                        continue;
                    }
                    // A heading on the first line opens its paragraph's note; one
                    // further down (after a line break) opens the next unit's.
                    let mut later = None;
                    if tag == "P" {
                        for (at, n, title) in note_headings(&text) {
                            let current = later.as_ref().or(self.note.as_ref()).map(|(c, _)| *c);
                            if !next_note(current, n) {
                                continue;
                            }
                            if at == 0 && later.is_none() {
                                self.note = Some((n, title));
                            } else {
                                later = Some((n, title));
                            }
                        }
                    }
                    let section = self.section();
                    self.out.push(Unit { table: None, node: child, tag: tag.into(), text, line, section });
                    self.nested_tables(child);
                    if later.is_some() {
                        self.note = later;
                    }
                } else {
                    let region = match tag {
                        "COVER" => Some("표지".to_owned()),
                        "TOC" => Some("목차".to_owned()),
                        t if t == "PART" || t.starts_with("SECTION-") => tree
                            .child_tag(child, "TITLE")
                            .map(|n| collapse(&node_text(tree, n, false)))
                            .filter(|s| !s.is_empty()),
                        _ => None,
                    };
                    if let Some(region) = region {
                        let saved = (self.region.clone(), self.note.clone());
                        // A new top-level region restarts note numbering; a nested one keeps it.
                        if !tag.starts_with("SECTION-") || tag == "SECTION-1" {
                            self.note = None;
                        }
                        self.region = region;
                        self.walk(child);
                        if tag == "SECTION-1" || tag == "COVER" || tag == "TOC" || tag == "PART" {
                            (self.region, self.note) = saved;
                        }
                    } else {
                        self.walk(child);
                    }
                }
            }
        }
    }
    let mut w = Walk { doc, region: String::new(), note: None, out: Vec::new() };
    w.walk(doc.root());
    w.out
}

// ---------------------------------------------------------------------------
// normalization and similarity

/// Row and column matching key: whitespace, footnote markers, and the
/// spelling of "주식회사" do not distinguish entries.
fn match_key(text: &str) -> String {
    let mut s = compact(text);
    for form in ["주식회사", "(주)", "（주）"] {
        s = s.replace(form, "㈜");
    }
    let mut out = String::with_capacity(s.len());
    let chars: Vec<char> = s.chars().collect();
    let mut i = 0;
    while i < chars.len() {
        // (*1) (*2) (주1) and similar markers.
        if chars[i] == '(' {
            let mut j = i + 1;
            if j < chars.len() && (chars[j] == '*' || chars[j] == '주') {
                j += 1;
                let digits = chars[j..].iter().take_while(|c| c.is_ascii_digit()).count();
                if digits > 0 && chars.get(j + digits) == Some(&')') {
                    i = j + digits + 1;
                    continue;
                }
            }
        }
        out.push(chars[i]);
        i += 1;
    }
    out
}

fn ratio(a: &str, b: &str) -> f64 {
    let (a, b): (Vec<char>, Vec<char>) = (a.chars().collect(), b.chars().collect());
    Matcher::new(&a, &b).ratio()
}

fn trigrams(text: &str) -> HashSet<(char, char, char)> {
    let c: Vec<char> = compact(text).chars().collect();
    if c.len() < 3 {
        return c.iter().map(|&x| (x, '\0', '\0')).collect();
    }
    c.windows(3).map(|w| (w[0], w[1], w[2])).collect()
}

/// Largest changed region (left x right keys) searched for similar pairs.
const MAX_PAIRING_WORK: usize = 40_000;

/// Pair two key lists in order: exact runs first, then within each changed
/// region the most similar keys (ratio >= 0.5), keeping order on the left.
fn pair_keys(a: &[String], b: &[String]) -> Vec<(Option<usize>, Option<usize>)> {
    let mut out = Vec::new();
    for (tag, i1, i2, j1, j2) in Matcher::new(a, b).opcodes() {
        if tag == Tag::Equal {
            out.extend((0..i2 - i1).map(|k| (Some(i1 + k), Some(j1 + k))));
            continue;
        }
        if (i2 - i1) * (j2 - j1) > MAX_PAIRING_WORK {
            // Too large to search: pair by position, the rest one-sided.
            let n = (i2 - i1).min(j2 - j1);
            out.extend((0..n).map(|k| (Some(i1 + k), Some(j1 + k))));
            out.extend((i1 + n..i2).map(|i| (Some(i), None)));
            out.extend((j1 + n..j2).map(|j| (None, Some(j))));
            continue;
        }
        let mut cand: Vec<(f64, usize, usize)> = Vec::new();
        for (i, ka) in a.iter().enumerate().take(i2).skip(i1) {
            for (j, kb) in b.iter().enumerate().take(j2).skip(j1) {
                if ka.is_empty() || kb.is_empty() {
                    continue;
                }
                let r = ratio(ka, kb);
                if r >= 0.5 {
                    cand.push((r, i, j));
                }
            }
        }
        cand.sort_by(|x, y| y.0.total_cmp(&x.0).then(x.1.cmp(&y.1)));
        let (mut left, mut used): (HashMap<usize, usize>, HashSet<usize>) = (HashMap::new(), HashSet::new());
        for (_, i, j) in cand {
            if left.contains_key(&i) || used.contains(&j) {
                continue;
            }
            // Keep the pairing monotonic so rows never cross.
            let crosses = left.iter().any(|(&pi, &pj)| (pi < i) != (pj < j));
            if crosses {
                continue;
            }
            left.insert(i, j);
            used.insert(j);
        }
        let mut pending: Vec<usize> = (j1..j2).filter(|j| !used.contains(j)).collect();
        for i in i1..i2 {
            if let Some(&j) = left.get(&i) {
                // Unpaired right-hand keys that come before this pair go first.
                let (before, after): (Vec<usize>, Vec<usize>) = pending.into_iter().partition(|&p| p < j);
                out.extend(before.into_iter().map(|p| (None, Some(p))));
                pending = after;
                out.push((Some(i), Some(j)));
            } else {
                out.push((Some(i), None));
            }
        }
        out.extend(pending.into_iter().map(|p| (None, Some(p))));
    }
    out
}

// ---------------------------------------------------------------------------
// numbers

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
struct Num {
    mantissa: i128,
    scale: u32,
    percent: bool,
}

/// A figure as preparers write it: `1,234`, `(1,234)`, `-1,234`, `△1,234`,
/// `12.5%`, or `-` for nil.
fn parse_number(text: &str) -> Option<Num> {
    let s = compact(text);
    if s == "-" || s == "－" {
        return Some(Num { mantissa: 0, scale: 0, percent: false });
    }
    let (s, percent) = match s.strip_suffix('%') {
        Some(rest) => (rest.to_owned(), true),
        None => (s, false),
    };
    let (body, neg) = if let Some(inner) = s.strip_prefix('(').and_then(|r| r.strip_suffix(')')) {
        (inner.to_owned(), true)
    } else if let Some(rest) = s.strip_prefix(['-', '△', '▲', '－']) {
        (rest.to_owned(), true)
    } else {
        (s, false)
    };
    let body = body.replace(',', "");
    let (int, frac) = body.split_once('.').unwrap_or((&body, ""));
    if int.is_empty() && frac.is_empty() {
        return None;
    }
    if !int.bytes().all(|b| b.is_ascii_digit())
        || !frac.bytes().all(|b| b.is_ascii_digit())
        // 18 digits keep every rescaled value inside i128.
        || int.len() + frac.len() > 18
    {
        return None;
    }
    let digits = format!("{int}{frac}");
    let mantissa: i128 = if digits.is_empty() { 0 } else { digits.parse().ok()? };
    Some(Num { mantissa: if neg { -mantissa } else { mantissa }, scale: u32::try_from(frac.len()).ok()?, percent })
}

fn rescale(n: Num, scale: u32) -> i128 {
    n.mantissa * 10i128.pow(scale - n.scale)
}

fn group_thousands(digits: &str) -> String {
    let mut out = String::new();
    for (i, ch) in digits.chars().enumerate() {
        if i > 0 && (digits.len() - i).is_multiple_of(3) {
            out.push(',');
        }
        out.push(ch);
    }
    out
}

/// `other - base` with a sign and thousands separators, or `None` when either is not a number.
fn delta(base: &str, other: &str) -> Option<String> {
    let (a, b) = (parse_number(base)?, parse_number(other)?);
    let scale = a.scale.max(b.scale);
    let d = rescale(b, scale) - rescale(a, scale);
    let abs = d.unsigned_abs().to_string();
    let abs = format!("{abs:0>width$}", width = scale as usize + 1);
    let (int, frac) = abs.split_at(abs.len() - scale as usize);
    let mut s = String::from(match d.cmp(&0) {
        std::cmp::Ordering::Less => "-",
        std::cmp::Ordering::Greater => "+",
        std::cmp::Ordering::Equal => "",
    });
    s.push_str(&group_thousands(int));
    if !frac.is_empty() {
        s.push('.');
        s.push_str(frac);
    }
    if a.percent && b.percent {
        s.push_str("%p");
    }
    Some(s)
}

// ---------------------------------------------------------------------------
// result model

#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct Side {
    pub file: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub document: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub type_code: Option<String>,
    pub tables: usize,
    pub text_blocks: usize,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "camelCase")]
pub enum Status {
    Same,
    Changed,
    OnlyBase,
    OnlyOther,
}

#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct Location {
    #[serde(skip_serializing_if = "Option::is_none")]
    pub table: Option<usize>,
    pub line: usize,
    pub tag: String,
    /// The table's first text, or the paragraph's opening words.
    pub label: String,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "camelCase")]
pub enum CellKind {
    /// Both are numbers and differ.
    Value,
    /// The same number written differently (`1,000` and `1000`, `-` and an empty cell).
    Format,
    Text,
}

#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct CellChange {
    pub kind: CellKind,
    pub base: String,
    pub other: String,
    pub row_label: String,
    pub column: String,
    pub base_text: String,
    pub other_text: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub delta: Option<String>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "camelCase")]
pub enum RowKind {
    OnlyBase,
    OnlyOther,
    /// The same entry at a different position.
    Moved,
}

#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct RowChange {
    pub kind: RowKind,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub base: Option<usize>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub other: Option<usize>,
    pub text: String,
}

#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct ColumnChange {
    pub kind: RowKind,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub base: Option<usize>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub other: Option<usize>,
    pub header: String,
}

#[derive(Debug, Clone, Serialize)]
pub struct Fragment {
    pub base: String,
    pub other: String,
}

#[derive(Debug, Clone, Serialize)]
pub struct TextChange {
    pub base: String,
    pub other: String,
    pub fragments: Vec<Fragment>,
}

/// Grid alignment kept for rendering: which rows and columns correspond.
#[derive(Debug, Clone, Default)]
pub struct TableAlignment {
    /// 0-based row pairs.
    pub rows: Vec<(Option<usize>, Option<usize>)>,
    pub moved: HashSet<(usize, usize)>,
    /// 1-based base column -> other column.
    pub cols: HashMap<usize, usize>,
    /// Changed cells as (base row, base col) and (other row, other col), 1-based.
    pub changed: Vec<((usize, usize), (usize, usize))>,
}

#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct Item {
    pub kind: &'static str,
    pub status: Status,
    pub section: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub base: Option<Location>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub other: Option<Location>,
    #[serde(skip_serializing_if = "Vec::is_empty")]
    pub cells: Vec<CellChange>,
    #[serde(skip_serializing_if = "Vec::is_empty")]
    pub rows: Vec<RowChange>,
    #[serde(skip_serializing_if = "Vec::is_empty")]
    pub columns: Vec<ColumnChange>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub text: Option<TextChange>,
    /// The pair sits at different places in the two documents (a reordered table).
    #[serde(skip_serializing_if = "std::ops::Not::not")]
    pub moved: bool,
    #[serde(skip)]
    pub alignment: Option<TableAlignment>,
    #[serde(skip)]
    base_node: Option<NodeId>,
    #[serde(skip)]
    other_node: Option<NodeId>,
}

#[derive(Debug, Clone, Default, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct Summary {
    pub items: usize,
    pub same: usize,
    pub changed: usize,
    pub only_base: usize,
    pub only_other: usize,
    pub tables_changed: usize,
    pub text_changed: usize,
    pub value_changes: usize,
    pub text_cell_changes: usize,
    pub format_changes: usize,
    pub row_changes: usize,
    pub column_changes: usize,
    pub moved_tables: usize,
}

#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct Comparison {
    pub base: Side,
    pub other: Side,
    pub summary: Summary,
    pub items: Vec<Item>,
}

// ---------------------------------------------------------------------------
// table comparison

/// A table's cells on its grid. Each cell's text is extracted once; a span
/// shares it across the positions it covers, so wide spans cost no extra text.
struct Grid<'t> {
    table: &'t Table,
    /// Text of each cell, by its index in `Table::cells`.
    texts: Vec<String>,
    rows: usize,
    header_rows: usize,
}

impl Grid<'_> {
    /// Text of the cell that starts at a 0-based position; `None` where a span covers it.
    fn origin(&self, r: usize, c: usize) -> Option<&str> {
        let i = (*self.table.grid.get(r)?.get(c)?)?;
        let cell = &self.table.cells[i];
        ((cell.row, cell.col) == (r + 1, c + 1)).then(|| self.texts[i].as_str())
    }

    /// Text of whichever cell covers a 0-based position, for labels.
    fn covering(&self, r: usize, c: usize) -> &str {
        self.table.grid.get(r).and_then(|row| row.get(c)).copied().flatten().map_or("", |i| self.texts[i].as_str())
    }

    fn row(&self, r: usize) -> impl Iterator<Item = &str> {
        (0..self.table.ncols()).filter_map(move |c| self.origin(r, c))
    }
}

fn grid<'t>(doc: &Doc, t: &'t Table) -> Grid<'t> {
    let tree = &doc.tree;
    if t.oversized || t.grid.len() != t.rows.len() {
        // No usable grid (oversized or irregular): compare the table as a whole only.
        return Grid { table: t, texts: Vec::new(), rows: 0, header_rows: 0 };
    }
    let texts = t.cells.iter().map(|c| own_text(tree, c.node)).collect();
    let nr = t.rows.len();
    let thead = t.rows.iter().take_while(|&&r| tree.node(r).parent.is_some_and(|p| tree.tag(p) == "THEAD")).count();
    let header_rows = if thead > 0 { thead } else { nr.min(2) };
    Grid { table: t, texts, rows: nr, header_rows }
}

fn row_key(g: &Grid, r: usize) -> String {
    g.row(r).map(match_key).find(|k| !k.is_empty()).unwrap_or_default()
}

fn row_label(g: &Grid, r: usize) -> String {
    let text = (0..g.table.ncols()).map(|c| collapse(g.covering(r, c))).find(|t| !t.is_empty()).unwrap_or_default();
    prefix(&text, 60)
}

fn col_header(g: &Grid, c: usize) -> String {
    let parts: Vec<String> = (0..g.header_rows)
        .map(|r| collapse(g.covering(r, c)))
        .filter(|t| !t.is_empty() && parse_number(t).is_none())
        .collect();
    let mut out: Vec<String> = Vec::new();
    for p in parts {
        if out.last() != Some(&p) {
            out.push(p);
        }
    }
    prefix(&out.join(" / "), 60)
}

fn row_text(g: &Grid, r: usize) -> String {
    prefix(&g.row(r).map(collapse).filter(|t| !t.is_empty()).collect::<Vec<_>>().join(" | "), 160)
}

fn cell_change(base: &str, other: &str) -> Option<(CellKind, Option<String>)> {
    if compact(base) == compact(other) {
        return None;
    }
    // Only footnote markers or the spelling of 주식회사 differ.
    if parse_number(base).is_none() && parse_number(other).is_none() && match_key(base) == match_key(other) {
        return Some((CellKind::Format, None));
    }
    match (parse_number(base), parse_number(other)) {
        (Some(a), Some(b)) => {
            let s = a.scale.max(b.scale);
            if rescale(a, s) == rescale(b, s) && a.percent == b.percent {
                Some((CellKind::Format, None))
            } else {
                Some((CellKind::Value, delta(base, other)))
            }
        }
        (Some(n), None) | (None, Some(n))
            if n.mantissa == 0 && compact(if parse_number(base).is_some() { other } else { base }).is_empty() =>
        {
            Some((CellKind::Format, None))
        }
        _ => Some((CellKind::Text, None)),
    }
}

fn compare_tables(bdoc: &Doc, bt: &Table, odoc: &Doc, ot: &Table, item: &mut Item) {
    let (bg, og) = (grid(bdoc, bt), grid(odoc, ot));
    let bk: Vec<String> = (0..bg.rows).map(|r| row_key(&bg, r)).collect();
    let ok: Vec<String> = (0..og.rows).map(|r| row_key(&og, r)).collect();
    let mut rows = pair_keys(&bk, &ok);
    // A row that moved: unpaired on both sides with the same key.
    let mut moved = HashSet::new();
    let lefts: Vec<usize> = rows.iter().enumerate().filter(|(_, p)| p.1.is_none()).map(|(k, _)| k).collect();
    let rights: Vec<usize> = rows.iter().enumerate().filter(|(_, p)| p.0.is_none()).map(|(k, _)| k).collect();
    let mut drop = HashSet::new();
    for &l in &lefts {
        let i = rows[l].0.expect("left entry");
        if bk[i].is_empty() {
            continue;
        }
        if let Some(&r) = rights.iter().find(|&&r| !drop.contains(&r) && ok[rows[r].1.expect("right entry")] == bk[i]) {
            let j = rows[r].1.expect("right entry");
            rows[l] = (Some(i), Some(j));
            drop.insert(r);
            moved.insert((i, j));
        }
    }
    let rows: Vec<(Option<usize>, Option<usize>)> =
        rows.into_iter().enumerate().filter(|(k, _)| !drop.contains(k)).map(|(_, p)| p).collect();
    // Columns: identity when the widths agree, else by header text.
    let (bn, on) = (bt.ncols(), ot.ncols());
    let mut cols: HashMap<usize, usize> = HashMap::new();
    if bn == on {
        cols.extend((1..=bn).map(|c| (c, c)));
    } else {
        let bh: Vec<String> = (0..bn).map(|c| match_key(&col_header(&bg, c))).collect();
        let oh: Vec<String> = (0..on).map(|c| match_key(&col_header(&og, c))).collect();
        for (a, b) in pair_keys(&bh, &oh) {
            if let (Some(a), Some(b)) = (a, b) {
                cols.insert(a + 1, b + 1);
            }
        }
        if bn > 0 && on > 0 && !cols.values().any(|&v| v == 1) {
            cols.insert(1, 1);
        }
        for c in 1..=bn {
            if !cols.contains_key(&c) {
                item.columns.push(ColumnChange {
                    kind: RowKind::OnlyBase,
                    base: Some(c),
                    other: None,
                    header: col_header(&bg, c - 1),
                });
            }
        }
        let mapped: HashSet<usize> = cols.values().copied().collect();
        for c in 1..=on {
            if !mapped.contains(&c) {
                item.columns.push(ColumnChange {
                    kind: RowKind::OnlyOther,
                    base: None,
                    other: Some(c),
                    header: col_header(&og, c - 1),
                });
            }
        }
    }
    let mut changed = Vec::new();
    let mut bcols: Vec<usize> = cols.keys().copied().collect();
    bcols.sort_unstable();
    for &(i, j) in &rows {
        match (i, j) {
            (Some(i), Some(j)) => {
                if moved.contains(&(i, j)) {
                    item.rows.push(RowChange {
                        kind: RowKind::Moved,
                        base: Some(i + 1),
                        other: Some(j + 1),
                        text: row_text(&bg, i),
                    });
                }
                for &bc in &bcols {
                    let oc = cols[&bc];
                    let (a, b) = (bg.origin(i, bc - 1), og.origin(j, oc - 1));
                    if a.is_none() && b.is_none() {
                        continue;
                    }
                    let (at, bt_) = (a.unwrap_or_default(), b.unwrap_or_default());
                    if let Some((kind, d)) = cell_change(at, bt_) {
                        item.cells.push(CellChange {
                            kind,
                            base: format!("R{}C{bc}", i + 1),
                            other: format!("R{}C{oc}", j + 1),
                            row_label: row_label(&bg, i),
                            column: col_header(&bg, bc - 1),
                            base_text: at.to_owned(),
                            other_text: bt_.to_owned(),
                            delta: d,
                        });
                        changed.push(((i + 1, bc), (j + 1, oc)));
                    }
                }
            }
            (Some(i), None) => {
                item.rows.push(RowChange {
                    kind: RowKind::OnlyBase,
                    base: Some(i + 1),
                    other: None,
                    text: row_text(&bg, i),
                });
            }
            (None, Some(j)) => {
                item.rows.push(RowChange {
                    kind: RowKind::OnlyOther,
                    base: None,
                    other: Some(j + 1),
                    text: row_text(&og, j),
                });
            }
            (None, None) => {}
        }
    }
    item.alignment = Some(TableAlignment { rows, moved, cols, changed });
}

// ---------------------------------------------------------------------------
// text comparison

/// Words with the whitespace that follows each, so a rendering keeps layout.
pub fn words(text: &str) -> Vec<(String, String)> {
    let mut out: Vec<(String, String)> = Vec::new();
    let mut word = String::new();
    let mut space = String::new();
    for ch in text.chars() {
        if ch.is_whitespace() {
            space.push(ch);
        } else {
            if !space.is_empty() {
                out.push((std::mem::take(&mut word), std::mem::take(&mut space)));
            }
            word.push(ch);
        }
    }
    if !word.is_empty() || !space.is_empty() {
        out.push((word, space));
    }
    out
}

fn text_fragments(base: &str, other: &str) -> Vec<Fragment> {
    let (a, b): (Vec<String>, Vec<String>) =
        (words(base).into_iter().map(|w| w.0).collect(), words(other).into_iter().map(|w| w.0).collect());
    let mut out = Vec::new();
    for (tag, i1, i2, j1, j2) in Matcher::new(&a, &b).opcodes() {
        if tag == Tag::Equal {
            continue;
        }
        let (x, y) = (a[i1..i2].join(" "), b[j1..j2].join(" "));
        if compact(&x) != compact(&y) {
            out.push(Fragment { base: x, other: y });
        }
    }
    out
}

// ---------------------------------------------------------------------------
// alignment

struct Features {
    /// Characters of content: tiny units such as a "(단위:천원)" table pair weakly.
    size: usize,
    cells: HashMap<String, usize>,
    total: usize,
    head: String,
    ncols: usize,
    grams: HashSet<(char, char, char)>,
    key: String,
}

fn features(doc: &Doc, u: &Unit) -> Features {
    if let Some(index) = u.table {
        let t = &doc.tables[index - 1];
        let mut cells: HashMap<String, usize> = HashMap::new();
        let mut all = Vec::new();
        for c in &t.cells {
            let k = compact(&own_text(&doc.tree, c.node));
            all.push(k.clone());
            if !k.is_empty() {
                *cells.entry(k).or_default() += 1;
            }
        }
        let head = t
            .cells
            .iter()
            .filter(|c| c.row == 1)
            .map(|c| compact(&own_text(&doc.tree, c.node)))
            .collect::<Vec<_>>()
            .join("|");
        let total = cells.values().sum();
        let size = all.iter().map(|k| k.chars().count()).sum();
        Features {
            size,
            cells,
            total,
            head,
            ncols: t.ncols(),
            grams: HashSet::new(),
            key: format!("T\u{1f}{}", all.join("\u{1f}")),
        }
    } else {
        Features {
            size: compact(&u.text).chars().count(),
            cells: HashMap::new(),
            total: 0,
            head: String::new(),
            ncols: 0,
            grams: trigrams(&u.text),
            key: format!("P\u{1f}{}", compact(&u.text)),
        }
    }
}

fn similarity(a: &Features, ua: &Unit, b: &Features, ub: &Unit) -> f64 {
    match (ua.table, ub.table) {
        (Some(_), Some(_)) => {
            let inter: usize = a.cells.iter().map(|(k, &n)| n.min(*b.cells.get(k).unwrap_or(&0))).sum();
            let mut s = inter as f64 / a.total.max(b.total).max(1) as f64;
            if a.ncols == b.ncols {
                s += 0.05;
            }
            if !a.head.is_empty() && a.head == b.head {
                s += 0.1;
            }
            s
        }
        (None, None) => {
            let inter = a.grams.intersection(&b.grams).count();
            let union = a.grams.len() + b.grams.len() - inter;
            let jaccard = inter as f64 / union.max(1) as f64;
            // A heading one side keeps apart and the other joins to the next
            // paragraph: the shorter text lies almost wholly inside the longer.
            let shorter = a.grams.len().min(b.grams.len());
            let contained = if shorter >= 4 { 0.8 * inter as f64 / shorter as f64 } else { 0.0 };
            jaccard.max(contained)
        }
        _ => 0.0,
    }
}

/// Above this many candidate pairs in one changed region, the region is
/// paired by position within each kind instead of by similarity.
const MAX_REGION_WORK: usize = 2_000_000;
const MIN_SIMILARITY: f64 = 0.3;
/// Units with fewer content characters never anchor the alignment on their own.
const MIN_ANCHOR_SIZE: usize = 15;

fn align_region(
    fa: &[Features],
    ua: &[Unit],
    fb: &[Features],
    ub: &[Unit],
    out: &mut Vec<(Option<usize>, Option<usize>)>,
    oa: usize,
    ob: usize,
) {
    let (m, n) = (ua.len(), ub.len());
    if m * n > MAX_REGION_WORK {
        out.extend((0..m).map(|i| (Some(oa + i), None)));
        out.extend((0..n).map(|j| (None, Some(ob + j))));
        return;
    }
    // Similarity gates a pairing; the score it adds is weighted by how much
    // content the two units carry, so boilerplate never outranks a heading.
    let mut sim = vec![vec![0.0f64; n]; m];
    let mut gain = vec![vec![0.0f64; n]; m];
    for i in 0..m {
        for j in 0..n {
            sim[i][j] = similarity(&fa[i], &ua[i], &fb[j], &ub[j]);
            let size = fa[i].size.min(fb[j].size) as f64;
            gain[i][j] = sim[i][j] * (size / 30.0).clamp(0.2, 1.0);
        }
    }
    let mut score = vec![vec![0.0f64; n + 1]; m + 1];
    for i in (0..m).rev() {
        for j in (0..n).rev() {
            let mut best = score[i + 1][j].max(score[i][j + 1]);
            if sim[i][j] >= MIN_SIMILARITY {
                best = best.max(score[i + 1][j + 1] + gain[i][j]);
            }
            score[i][j] = best;
        }
    }
    let (mut i, mut j) = (0, 0);
    #[allow(clippy::float_cmp)] // retraces the exact choices the table recorded
    while i < m || j < n {
        if i < m && j < n && sim[i][j] >= MIN_SIMILARITY && score[i][j] == score[i + 1][j + 1] + gain[i][j] {
            out.push((Some(oa + i), Some(ob + j)));
            i += 1;
            j += 1;
        } else if i < m && (j == n || score[i][j] == score[i + 1][j]) {
            out.push((Some(oa + i), None));
            i += 1;
        } else {
            out.push((None, Some(ob + j)));
            j += 1;
        }
    }
}

/// Least similarity for pairing tables the alignment left one-sided.
const MOVED_SIMILARITY: f64 = 0.6;

/// Pair tables that moved past an anchor. Order-preserving alignment cannot
/// pair `[A, B]` with `[B', A]`: A anchors, and B and B' fall into different
/// regions. Pair such one-sided tables by similarity across the whole
/// document; the pair takes the base table's place. Returns the base units
/// paired this way.
fn pair_moved_tables(
    fa: &[Features],
    ua: &[Unit],
    fb: &[Features],
    ub: &[Unit],
    pairs: &mut Vec<(Option<usize>, Option<usize>)>,
) -> HashSet<usize> {
    let lefts: Vec<usize> = (0..pairs.len())
        .filter(|&k| matches!(pairs[k], (Some(i), None) if ua[i].table.is_some() && fa[i].size >= MIN_ANCHOR_SIZE))
        .collect();
    let rights: Vec<usize> = (0..pairs.len())
        .filter(|&k| matches!(pairs[k], (None, Some(j)) if ub[j].table.is_some() && fb[j].size >= MIN_ANCHOR_SIZE))
        .collect();
    let mut moved = HashSet::new();
    if lefts.is_empty() || rights.is_empty() || lefts.len() * rights.len() > MAX_PAIRING_WORK {
        return moved;
    }
    let mut cand: Vec<(f64, usize, usize)> = Vec::new();
    for &l in &lefts {
        let i = pairs[l].0.expect("left entry");
        for &r in &rights {
            let j = pairs[r].1.expect("right entry");
            let sim = similarity(&fa[i], &ua[i], &fb[j], &ub[j]);
            if sim >= MOVED_SIMILARITY {
                cand.push((sim, l, r));
            }
        }
    }
    cand.sort_by(|x, y| y.0.total_cmp(&x.0).then(x.1.cmp(&y.1)).then(x.2.cmp(&y.2)));
    let (mut used_l, mut used_r) = (HashSet::new(), HashSet::new());
    for (_, l, r) in cand {
        if used_l.contains(&l) || used_r.contains(&r) {
            continue;
        }
        used_l.insert(l);
        used_r.insert(r);
        pairs[l].1 = pairs[r].1;
        moved.insert(pairs[l].0.expect("left entry"));
    }
    let mut k = 0;
    pairs.retain(|_| {
        k += 1;
        !used_r.contains(&(k - 1))
    });
    moved
}

fn location(doc: &Doc, u: &Unit) -> Location {
    let label = match u.table {
        Some(index) => {
            let t = &doc.tables[index - 1];
            t.cells.iter().map(|c| collapse(&own_text(&doc.tree, c.node))).find(|s| !s.is_empty()).unwrap_or_default()
        }
        None => collapse(&u.text),
    };
    Location { table: u.table, line: u.line, tag: u.tag.clone(), label: prefix(&label, 80) }
}

fn side(file: &str, doc: &Doc, units: &[Unit]) -> Side {
    let head = header_info(doc);
    Side {
        file: file.to_owned(),
        document: head.document_name,
        type_code: head.document_code,
        tables: doc.tables.len(),
        text_blocks: units.iter().filter(|u| u.table.is_none()).count(),
    }
}

/// Compare `other` against `base`. Every aligned or unmatched table and text
/// block becomes one item, in document order.
pub fn compare(base_file: &str, base: &Doc, other_file: &str, other: &Doc) -> Comparison {
    let (ua, ub) = (units(base), units(other));
    let fa: Vec<Features> = ua.iter().map(|u| features(base, u)).collect();
    let fb: Vec<Features> = ub.iter().map(|u| features(other, u)).collect();
    // Identical units anchor the alignment, except tiny ones ("(단위:천원)",
    // "<당분기>") that repeat everywhere: those are left to the weighted pass.
    let anchor = |side: char, f: &[Features]| -> Vec<String> {
        f.iter()
            .enumerate()
            .map(|(k, x)| if x.size < MIN_ANCHOR_SIZE { format!("{side}{k}\u{1e}") } else { x.key.clone() })
            .collect()
    };
    let (ka, kb) = (anchor('a', &fa), anchor('b', &fb));
    let mut pairs = Vec::new();
    for (tag, i1, i2, j1, j2) in Matcher::new(&ka, &kb).opcodes() {
        if tag == Tag::Equal {
            pairs.extend((0..i2 - i1).map(|k| (Some(i1 + k), Some(j1 + k))));
        } else {
            align_region(&fa[i1..i2], &ua[i1..i2], &fb[j1..j2], &ub[j1..j2], &mut pairs, i1, j1);
        }
    }
    let moved = pair_moved_tables(&fa, &ua, &fb, &ub, &mut pairs);
    let mut items = Vec::new();
    let mut summary = Summary::default();
    // Sections follow the base document; an item only the other side has is
    // listed under the base section it falls in, so headings never alternate.
    let mut base_section: Option<String> = None;
    for (i, j) in pairs {
        let (a, b) = (i.map(|i| &ua[i]), j.map(|j| &ub[j]));
        let u = a.or(b).expect("a pair has a side");
        let section = match (a, &base_section) {
            (Some(a), _) => {
                base_section = Some(a.section.clone());
                a.section.clone()
            }
            (None, Some(s)) => s.clone(),
            (None, None) => b.expect("side").section.clone(),
        };
        let mut item = Item {
            kind: if u.table.is_some() { "table" } else { "text" },
            status: Status::Same,
            section,
            base: a.map(|a| location(base, a)),
            other: b.map(|b| location(other, b)),
            cells: Vec::new(),
            rows: Vec::new(),
            columns: Vec::new(),
            text: None,
            moved: i.is_some_and(|i| moved.contains(&i)),
            alignment: None,
            base_node: a.map(|a| a.node),
            other_node: b.map(|b| b.node),
        };
        match (a, b) {
            (Some(a), Some(b)) => {
                if let (Some(ta), Some(tb)) = (a.table, b.table) {
                    compare_tables(base, &base.tables[ta - 1], other, &other.tables[tb - 1], &mut item);
                    if !item.cells.is_empty() || !item.rows.is_empty() || !item.columns.is_empty() {
                        item.status = Status::Changed;
                    }
                } else if compact(&a.text) != compact(&b.text) {
                    item.status = Status::Changed;
                    item.text = Some(TextChange {
                        base: a.text.clone(),
                        other: b.text.clone(),
                        fragments: text_fragments(&a.text, &b.text),
                    });
                }
            }
            (Some(_), None) => item.status = Status::OnlyBase,
            _ => item.status = Status::OnlyOther,
        }
        if item.moved {
            item.status = Status::Changed;
            summary.moved_tables += 1;
        }
        summary.items += 1;
        match item.status {
            Status::Same => summary.same += 1,
            Status::Changed => {
                summary.changed += 1;
                if item.kind == "table" {
                    summary.tables_changed += 1;
                } else {
                    summary.text_changed += 1;
                }
            }
            Status::OnlyBase => summary.only_base += 1,
            Status::OnlyOther => summary.only_other += 1,
        }
        for c in &item.cells {
            match c.kind {
                CellKind::Value => summary.value_changes += 1,
                CellKind::Text => summary.text_cell_changes += 1,
                CellKind::Format => summary.format_changes += 1,
            }
        }
        summary.row_changes += item.rows.len();
        summary.column_changes += item.columns.len();
        items.push(item);
    }
    Comparison { base: side(base_file, base, &ua), other: side(other_file, other, &ub), summary, items }
}

// ---------------------------------------------------------------------------
// text rendering

fn status_mark(s: Status) -> char {
    match s {
        Status::Same => '=',
        Status::Changed => '~',
        Status::OnlyBase => '-',
        Status::OnlyOther => '+',
    }
}

fn loc_label(l: Option<&Location>) -> String {
    match l {
        None => "·".into(),
        Some(l) => match l.table {
            Some(t) => format!("T{t}"),
            None => format!("L{}", l.line),
        },
    }
}

/// Compact listing: one line per item, then its cell, row, column, and wording changes.
pub fn render_text(c: &Comparison, items: &[&Item]) -> String {
    let mut out = String::new();
    let _ = writeln!(out, "# base: {} ({} tables)", c.base.file, c.base.tables);
    let _ = writeln!(out, "# other: {} ({} tables)", c.other.file, c.other.tables);
    let s = &c.summary;
    let _ = writeln!(
        out,
        "# {} items: {} same, {} changed, {} only in base, {} only in other; {} value, {} text, {} format cell change(s), {} row change(s), {} moved table(s)",
        s.items,
        s.same,
        s.changed,
        s.only_base,
        s.only_other,
        s.value_changes,
        s.text_cell_changes,
        s.format_changes,
        s.row_changes,
        s.moved_tables
    );
    let mut section = None;
    for item in items {
        if section != Some(&item.section) {
            let _ = writeln!(out, "\n[{}]", item.section);
            section = Some(&item.section);
        }
        let label = item.base.as_ref().or(item.other.as_ref()).map(|l| l.label.clone()).unwrap_or_default();
        let _ = writeln!(
            out,
            "{} {} <-> {}{}  {}",
            status_mark(item.status),
            loc_label(item.base.as_ref()),
            loc_label(item.other.as_ref()),
            if item.moved { " (moved)" } else { "" },
            prefix(&label, 60)
        );
        for col in &item.columns {
            let (side, n) = match col.kind {
                RowKind::OnlyBase => ("base", col.base),
                _ => ("other", col.other),
            };
            let _ = writeln!(out, "    column only in {side}: C{} {:?}", n.unwrap_or(0), col.header);
        }
        for row in &item.rows {
            let line = match row.kind {
                RowKind::OnlyBase => format!("- base R{} {:?}", row.base.unwrap_or(0), row.text),
                RowKind::OnlyOther => format!("+ other R{} {:?}", row.other.unwrap_or(0), row.text),
                RowKind::Moved => {
                    format!("~ moved R{} -> R{} {:?}", row.base.unwrap_or(0), row.other.unwrap_or(0), row.text)
                }
            };
            let _ = writeln!(out, "    {line}");
        }
        for cell in &item.cells {
            let d = cell.delta.as_ref().map(|d| format!("  Δ {d}")).unwrap_or_default();
            let kind = match cell.kind {
                CellKind::Value => "",
                CellKind::Format => " [format]",
                CellKind::Text => " [text]",
            };
            let _ = writeln!(
                out,
                "    {} -> {} {} / {}: {:?} -> {:?}{d}{kind}",
                cell.base,
                cell.other,
                cell.row_label,
                cell.column,
                collapse(&cell.base_text),
                collapse(&cell.other_text)
            );
        }
        if let Some(t) = &item.text {
            for f in t.fragments.iter().take(20) {
                let _ = writeln!(out, "    base {:?} -> other {:?}", f.base, f.other);
            }
        }
    }
    out
}

// ---------------------------------------------------------------------------
// HTML report
//
// Two panes, one per document, rendered at DART's own sizes (column widths,
// row heights, USERMARK fonts) and aligned item by item: each item is one
// block in each pane, and the page script gives both blocks the taller one's
// height, so the panes scroll together line for line.

fn esc(text: &str) -> String {
    let mut s = String::with_capacity(text.len());
    for ch in text.chars() {
        match ch {
            '&' => s.push_str("&amp;"),
            '<' => s.push_str("&lt;"),
            '>' => s.push_str("&gt;"),
            '"' => s.push_str("&quot;"),
            '\'' => s.push_str("&#39;"),
            _ => s.push(ch),
        }
    }
    s
}

fn esc_lines(text: &str) -> String {
    esc(text).replace('\n', "<br>")
}

const BATANG: &str = "Batang,'바탕',AppleMyungjo,serif";
const GULIM: &str = "Gulim,'굴림',AppleGothic,sans-serif";

fn hex6(s: &str) -> Option<&str> {
    (s.len() == 6 && s.bytes().all(|b| b.is_ascii_hexdigit())).then_some(s)
}

/// CSS for a `USERMARK` (see the format reference): `F-n` size in points,
/// `F-BTn`/`F-GLn` family and size, `B`/`!B` weight, `0X…` text and `BC0X…`
/// background color, `A-L`/`A-C`/`A-R` alignment. Unknown tokens are ignored and
/// every value is validated, so the result is safe inside a style attribute.
fn usermark_css(mark: &str) -> String {
    let mut s = String::new();
    for t in mark.split_whitespace() {
        if let Some(rest) = t.strip_prefix("BC0X") {
            if let Some(hex) = hex6(rest) {
                let _ = write!(s, "background-color:#{hex};");
            }
        } else if let Some(rest) = t.strip_prefix("0X") {
            if let Some(hex) = hex6(rest) {
                let _ = write!(s, "color:#{hex};");
            }
        } else if let Some(rest) = t.strip_prefix("F-") {
            let (family, size) = if let Some(n) = rest.strip_prefix("BT") {
                (Some(BATANG), n)
            } else if let Some(n) = rest.strip_prefix("GL") {
                (Some(GULIM), n)
            } else {
                (None, rest)
            };
            if let Some(f) = family {
                let _ = write!(s, "font-family:{f};");
            }
            if let Ok(n) = size.parse::<u32>()
                && (6..=48).contains(&n)
            {
                let _ = write!(s, "font-size:{n}pt;");
            }
        } else {
            s.push_str(match t {
                "B" => "font-weight:700;",
                "!B" => "font-weight:400;",
                "A-L" => "text-align:left;",
                "A-C" => "text-align:center;",
                "A-R" => "text-align:right;",
                _ => "",
            });
        }
    }
    s
}

/// The node's own `USERMARK`, or else that of the `SPAN` or `P` holding most
/// of its text, outside nested tables. Rendering uses one style per block or
/// cell, so a minor inline run does not restyle the whole block.
fn effective_mark(doc: &Doc, node: NodeId) -> Option<&str> {
    fn walk<'d>(doc: &'d Doc, node: NodeId, best: &mut Option<(&'d str, usize)>) {
        for &child in doc.tree.children(node) {
            let tag = doc.tree.tag(child);
            if tag == "TABLE" || is_scaffold(tag) {
                continue;
            }
            match doc.attr(child, "USERMARK").filter(|m| !m.trim().is_empty()) {
                Some(m) if tag == "SPAN" || tag == "P" => {
                    let len = own_text(&doc.tree, child).chars().count();
                    if best.is_none_or(|b| len > b.1) {
                        *best = Some((m, len));
                    }
                }
                _ => walk(doc, child, best),
            }
        }
    }
    if let Some(m) = doc.attr(node, "USERMARK").filter(|m| !m.trim().is_empty()) {
        return Some(m);
    }
    let mut best = None;
    walk(doc, node, &mut best);
    let total = own_text(&doc.tree, node).chars().count();
    best.filter(|&(_, len)| len * 2 > total).map(|(m, _)| m)
}

/// Block or cell style: its effective `USERMARK`, then explicit `ALIGN`/`VALIGN`.
fn node_css(doc: &Doc, node: NodeId) -> String {
    let mut style = effective_mark(doc, node).map(usermark_css).unwrap_or_default();
    if let Some(a) = doc.attr(node, "ALIGN") {
        let a = a.to_ascii_lowercase();
        if ["left", "right", "center", "justify"].contains(&a.as_str()) {
            let _ = write!(style, "text-align:{a};");
        }
    }
    if let Some(v) = doc.attr(node, "VALIGN") {
        let v = v.to_ascii_lowercase();
        if ["top", "middle", "bottom"].contains(&v.as_str()) {
            let _ = write!(style, "vertical-align:{v};");
        }
    }
    style
}

/// A positive pixel size attribute, bounded so a corrupt value cannot blow up the layout.
fn px(doc: &Doc, node: NodeId, name: &str) -> Option<i64> {
    doc.attr(node, name).and_then(crate::doc::attr_int).filter(|v| (1..=10_000).contains(v))
}

/// Per-cell marks for one side of a table pair, keyed by 1-based (row, col).
struct CellMarks {
    base_side: bool,
    class: HashMap<(usize, usize), &'static str>,
    pair: HashMap<(usize, usize), String>,
    title: HashMap<(usize, usize), String>,
    /// The paired cell's text, for text changes whose characters get marked inside the cell.
    counterpart: HashMap<(usize, usize), String>,
    row_class: HashMap<usize, &'static str>,
}

fn marks(item: &Item, index: usize, base_side: bool) -> CellMarks {
    let mut m = CellMarks {
        base_side,
        class: HashMap::new(),
        pair: HashMap::new(),
        title: HashMap::new(),
        counterpart: HashMap::new(),
        row_class: HashMap::new(),
    };
    let Some(al) = &item.alignment else { return m };
    for (k, &(b, o)) in al.changed.iter().enumerate() {
        let key = if base_side { b } else { o };
        m.class.insert(key, if base_side { "chg-b" } else { "chg-o" });
        m.pair.insert(key, format!("c{index}-{k}"));
        if let Some(c) = item.cells.get(k) {
            let mut t = format!("{} → {}", collapse(&c.base_text), collapse(&c.other_text));
            if let Some(d) = &c.delta {
                let _ = write!(t, "  (Δ {d})");
            }
            m.title.insert(key, t);
            // Numbers keep the whole-cell mark and Δ: marking digits of two different
            // amounts would scatter highlights that mean nothing.
            if c.kind == CellKind::Text {
                m.counterpart.insert(key, if base_side { c.other_text.clone() } else { c.base_text.clone() });
            }
        }
    }
    for &(i, j) in &al.rows {
        match (i, j) {
            (Some(i), None) if base_side => {
                m.row_class.insert(i + 1, "row-only-b");
            }
            (None, Some(j)) if !base_side => {
                m.row_class.insert(j + 1, "row-only-o");
            }
            (Some(i), Some(j)) if al.moved.contains(&(i, j)) => {
                m.row_class.insert(if base_side { i + 1 } else { j + 1 }, "row-moved");
            }
            _ => {}
        }
    }
    m
}

/// A table at DART's pixel sizes: declared column widths and row heights.
/// The pane scrolls sideways for a table wider than it.
fn render_table(doc: &Doc, table: &Table, marks: &CellMarks) -> String {
    let tree = &doc.tree;
    let mut out = String::new();
    let border = doc.attr(table.node, "BORDER").is_none_or(|b| b.trim() != "0");
    // Without COLGROUP widths the browser sizes the columns.
    let sized = !table.cols.is_empty() && table.cols.iter().all(|&w| (1..=10_000).contains(&w));
    let _ = write!(out, "<table class=\"dsd{}\"", if border { "" } else { " nb" });
    if sized {
        let _ = write!(out, " style=\"width:{}px\"><colgroup>", table.cols.iter().sum::<i64>());
        for &w in &table.cols {
            let _ = write!(out, "<col style=\"width:{w}px\">");
        }
        out.push_str("</colgroup>");
    } else {
        out.push('>');
    }
    let mut by_row: Vec<Vec<&crate::doc::Cell>> = vec![Vec::new(); table.rows.len() + 1];
    for cell in &table.cells {
        if let Some(row) = by_row.get_mut(cell.row) {
            row.push(cell);
        }
    }
    for (r, row_cells) in by_row.iter().enumerate().skip(1) {
        let _ = write!(out, "<tr class=\"{}\"", marks.row_class.get(&r).copied().unwrap_or(""));
        if let Some(h) = table.rows.get(r - 1).and_then(|&n| px(doc, n, "HEIGHT")) {
            let _ = write!(out, " style=\"height:{h}px\"");
        }
        out.push('>');
        for &cell in row_cells {
            let node = cell.node;
            let tag = if tree.tag(node) == "TH" { "th" } else { "td" };
            let key = (cell.row, cell.col);
            let mut attrs = String::new();
            if cell.colspan > 1 {
                let _ = write!(attrs, " colspan=\"{}\"", cell.colspan);
            }
            if cell.rowspan > 1 {
                let _ = write!(attrs, " rowspan=\"{}\"", cell.rowspan);
            }
            if let Some(c) = marks.class.get(&key) {
                let _ = write!(attrs, " class=\"{c}\"");
            }
            if let Some(p) = marks.pair.get(&key) {
                let _ = write!(attrs, " data-k=\"{p}\"");
            }
            if let Some(t) = marks.title.get(&key) {
                let _ = write!(attrs, " title=\"{}\"", esc(t));
            }
            let mut style = node_css(doc, node);
            let text = own_text(tree, node);
            if parse_number(&text).is_some() {
                style.push_str("white-space:nowrap;");
            }
            if !style.is_empty() {
                let _ = write!(attrs, " style=\"{style}\"");
            }
            let body = match marks.counterpart.get(&key) {
                Some(other) => render_words(&text, Some(other), marks.base_side),
                None => esc_lines(&text),
            };
            let _ = write!(out, "<{tag}{attrs}>{body}</{tag}>");
        }
        out.push_str("</tr>");
    }
    out.push_str("</table>");
    out
}

/// Replaced words that keep at least this share of the shorter side's characters are
/// marked character by character ("재고" → "재고(주9)"); below it they are rewrites,
/// and scattered character marks would only add noise.
const CHAR_MARK_SHARE: f64 = 0.5;

/// Characters of `words` (each word then its following space) to mark against `other`.
fn char_flags(words: &[(String, String)], other: &[(String, String)]) -> Vec<bool> {
    let flat = |ws: &[(String, String)]| ws.iter().flat_map(|(w, s)| w.chars().chain(s.chars())).collect::<Vec<_>>();
    let (a, b) = (flat(words), flat(other));
    let m = Matcher::new(&a, &b);
    let kept: usize = m.matching_blocks().iter().map(|&(_, _, n)| n).sum();
    let shorter = a.len().min(b.len()).max(1);
    if (kept as f64) < CHAR_MARK_SHARE * shorter as f64 {
        return vec![true; a.len()];
    }
    let mut flags = vec![false; a.len()];
    for (tag, i1, i2, _, _) in m.opcodes() {
        if tag != Tag::Equal {
            flags[i1..i2].fill(true);
        }
    }
    flags
}

/// A text block with changed text marked: `<del>` on the base side, `<ins>` on the other.
/// Words pair first; a replaced run of words is then narrowed to the characters that
/// differ, since Korean attaches particles and note references without a space.
fn render_words(text: &str, counterpart: Option<&str>, base_side: bool) -> String {
    let Some(other) = counterpart else { return esc_lines(text) };
    let (a, b) = (words(text), words(other));
    let (ka, kb): (Vec<&str>, Vec<&str>) =
        (a.iter().map(|w| w.0.as_str()).collect(), b.iter().map(|w| w.0.as_str()).collect());
    // One flag per character of each word of `a`, whitespace excluded.
    let mut flags: Vec<Vec<bool>> = a.iter().map(|(w, _)| vec![false; w.chars().count()]).collect();
    for (tag, i1, i2, j1, j2) in Matcher::new(&ka, &kb).opcodes() {
        match tag {
            Tag::Equal => {}
            Tag::Replace => {
                let mut run = char_flags(&a[i1..i2], &b[j1..j2]).into_iter();
                for (k, (_, space)) in a[i1..i2].iter().enumerate() {
                    for f in &mut flags[i1 + k] {
                        *f = run.next().unwrap_or(true);
                    }
                    run.by_ref().take(space.chars().count()).for_each(drop);
                }
            }
            _ => flags[i1..i2].iter_mut().for_each(|f| f.fill(true)),
        }
    }
    let mark = if base_side { "del" } else { "ins" };
    let mut out = String::new();
    for ((word, space), wf) in a.iter().zip(flags) {
        let mut open = false;
        for (ch, f) in word.chars().zip(wf) {
            if f != open {
                let _ = write!(out, "<{}{mark}>", if f { "" } else { "/" });
                open = f;
            }
            out.push_str(&esc(ch.encode_utf8(&mut [0; 4])));
        }
        if open {
            let _ = write!(out, "</{mark}>");
        }
        out.push_str(&esc_lines(space));
    }
    out
}

fn render_block(doc: &Doc, node: NodeId, item: &Item, base_side: bool) -> String {
    let class = match doc.tree.tag(node) {
        "TITLE" => "ttl",
        "COVER-TITLE" => "cttl",
        "WARNING" => "warn",
        _ => "p",
    };
    let text = own_text(&doc.tree, node);
    let counterpart = item.text.as_ref().map(|t| if base_side { t.other.as_str() } else { t.base.as_str() });
    format!(
        "<div class=\"{class}\" style=\"{}\">{}</div>",
        node_css(doc, node),
        render_words(&text, counterpart, base_side)
    )
}

fn render_side(doc: &Doc, node: Option<NodeId>, item: &Item, index: usize, base_side: bool) -> String {
    let Some(node) = node else { return String::new() };
    if let Some(t) = doc.table_for_node(node) {
        let label = if item.moved { format!("T{} · moved", t.index) } else { format!("T{}", t.index) };
        format!("<div class=\"loc\">{label}</div>{}", render_table(doc, t, &marks(item, index, base_side)))
    } else {
        render_block(doc, node, item, base_side)
    }
}

/// The item's change list for the details panel, or `None` when it has nothing to list.
fn change_list(item: &Item) -> Option<String> {
    let mut out = String::new();
    if let Some(t) = item.text.as_ref().filter(|t| !t.fragments.is_empty()) {
        out.push_str("<table class=\"cl\"><tr><th>base wording</th><th>other wording</th></tr>");
        for f in &t.fragments {
            let _ = write!(
                out,
                "<tr><td><del>{}</del></td><td><ins>{}</ins></td></tr>",
                esc_lines(&f.base),
                esc_lines(&f.other)
            );
        }
        out.push_str("</table>");
    }
    if !(item.cells.is_empty() && item.rows.is_empty() && item.columns.is_empty()) {
        out.push_str("<table class=\"cl\"><tr><th>base</th><th>other</th><th>row</th><th>column</th><th>base value</th><th>other value</th><th>Δ</th></tr>");
        for c in &item.columns {
            let (b, o) = (
                c.base.map(|v| format!("C{v}")).unwrap_or_default(),
                c.other.map(|v| format!("C{v}")).unwrap_or_default(),
            );
            let what = if c.kind == RowKind::OnlyBase { "column only in base" } else { "column only in other" };
            let _ = write!(
                out,
                "<tr class=\"st\"><td>{b}</td><td>{o}</td><td colspan=\"2\">{what}</td><td colspan=\"3\">{}</td></tr>",
                esc(&c.header)
            );
        }
        for r in &item.rows {
            let (b, o) = (
                r.base.map(|v| format!("R{v}")).unwrap_or_default(),
                r.other.map(|v| format!("R{v}")).unwrap_or_default(),
            );
            let what = match r.kind {
                RowKind::OnlyBase => "row only in base",
                RowKind::OnlyOther => "row only in other",
                RowKind::Moved => "row moved",
            };
            let _ = write!(
                out,
                "<tr class=\"st\"><td>{b}</td><td>{o}</td><td colspan=\"2\">{what}</td><td colspan=\"3\">{}</td></tr>",
                esc(&r.text)
            );
        }
        for (k, c) in item.cells.iter().enumerate() {
            let kind = match c.kind {
                CellKind::Value => "v",
                CellKind::Format => "f",
                CellKind::Text => "t",
            };
            let _ = write!(
                out,
                "<tr class=\"{kind}\" data-c=\"{k}\"><td>{}</td><td>{}</td><td>{}</td><td>{}</td><td>{}</td><td>{}</td><td class=\"d\">{}</td></tr>",
                c.base,
                c.other,
                esc(&c.row_label),
                esc(&c.column),
                esc_lines(&c.base_text),
                esc_lines(&c.other_text),
                esc(c.delta.as_deref().unwrap_or(""))
            );
        }
        out.push_str("</table>");
    }
    (!out.is_empty()).then_some(out)
}

const STYLE: &str = r#"
:root{--bg:#eceef1;--paper:#fff;--same:#f4f5f7;--ink:#1d2129;--muted:#6b7280;--line:#d3d7dd;--th:#e2e4e8;
--del:#ffd5d8;--del-ink:#9b1c1c;--ins:#d3f2db;--ins-ink:#1e6b33;--mov:#fff1b8;--accent:#2f5fd0;
--batang:Batang,'바탕',AppleMyungjo,serif;--gulim:Gulim,'굴림',AppleGothic,sans-serif}
*{box-sizing:border-box}
html,body{height:100%}
body{margin:0;display:flex;flex-direction:column;overflow:hidden;background:var(--bg);color:var(--ink);font:13px/1.5 "Malgun Gothic","맑은 고딕","Apple SD Gothic Neo",sans-serif}
.bar{display:flex;flex-wrap:wrap;align-items:center;gap:6px 16px;padding:6px 12px;background:var(--paper);border-bottom:1px solid var(--line)}
.bar h1{font-size:15px;margin:0}
.chips span{display:inline-block;margin-right:6px;padding:1px 8px;border-radius:10px;background:var(--bg);font-size:12px}
.ctl{display:flex;flex-wrap:wrap;align-items:center;gap:4px}
.ctl button,.ctl label{font:inherit;font-size:12px;padding:3px 10px;border:1px solid var(--line);border-radius:6px;background:var(--paper);color:var(--ink);cursor:pointer}
.ctl button:hover,.ctl label:hover{border-color:var(--accent)}
.ctl button[aria-pressed=true]{border-color:var(--accent);color:var(--accent)}
#at{min-width:72px;text-align:center;font-variant-numeric:tabular-nums;color:var(--muted)}
.main{flex:1;min-height:0;display:flex}
#toc{flex:none;width:220px;overflow:auto;padding:8px 6px;border-right:1px solid var(--line);font-size:12px}
body.notoc #toc{display:none}
#toc a{display:flex;justify-content:space-between;gap:6px;padding:3px 8px;border-radius:5px;color:var(--ink);text-decoration:none;cursor:pointer}
#toc a:hover{background:var(--paper)}
#toc a b{font-weight:600;color:var(--accent)}
#toc a.clean b{color:var(--muted);font-weight:400}
body.only #toc a.clean{opacity:.45;pointer-events:none}
.col{flex:1 1 0;min-width:0;display:flex;flex-direction:column;background:var(--paper)}
.col+.col{border-left:1px solid var(--line)}
.fname{flex:none;padding:6px 12px;border-bottom:1px solid var(--line);font:700 14px/1.4 var(--batang);white-space:nowrap;overflow:hidden;text-overflow:ellipsis}
.fname small{margin-right:8px;font:600 11px sans-serif}
.col.b .fname small{color:var(--del-ink)}.col.o .fname small{color:var(--ins-ink)}
.pane{flex:1;min-height:0;overflow:auto;position:relative}
.blk{width:max-content;min-width:100%;padding:6px 16px;background:var(--paper)}
.blk.same{background:var(--same)}
.blk.changed{box-shadow:inset 3px 0 var(--accent)}
.blk.onlyBase{background:#fff4f4;box-shadow:inset 3px 0 #e06a6a}
.blk.onlyOther{background:#f1fbf3;box-shadow:inset 3px 0 #46a35e}
.blk.void{background:repeating-linear-gradient(135deg,#f6f7f9 0 6px,#eceef1 6px 12px);box-shadow:none}
.blk.sec{padding:14px 16px 4px;font-weight:700;font-size:14px;color:#1f4fbf;border-top:1px solid var(--line)}
.blk.focus{outline:2px solid var(--accent);outline-offset:-2px}
.loc{font:11px/1.4 sans-serif;color:var(--muted);margin-bottom:2px}
.p,.ttl,.cttl,.warn{max-width:640px;white-space:pre-wrap;font-family:var(--batang);font-size:12pt;line-height:1.6}
.ttl{font-weight:700;font-size:13pt}
.cttl{font-weight:700;font-size:20pt;text-align:center;color:#1340a8}
.warn{color:#8a5a00}
del{background:var(--del);color:var(--del-ink);text-decoration:line-through}
ins{background:var(--ins);color:var(--ins-ink);text-decoration:none}
table.dsd{border-collapse:collapse;table-layout:fixed;margin:2px 0 4px;font-family:var(--gulim);font-size:11pt;line-height:1.45}
table.dsd td,table.dsd th{border:1px solid #7d848e;padding:2px 4px;white-space:pre-wrap;overflow-wrap:anywhere}
table.dsd th{background:var(--th);font-weight:400}
table.dsd.nb td,table.dsd.nb th{border-color:transparent}
td.chg-b,th.chg-b{background:var(--del)!important;color:var(--del-ink)}
td.chg-o,th.chg-o{background:var(--ins)!important;color:var(--ins-ink)}
td.chg-b del,th.chg-b del{background:#f3a6a6;font-weight:700}
td.chg-o ins,th.chg-o ins{background:#9edbab;font-weight:700}
td.hot,th.hot,tr.hot td{outline:2px solid var(--accent);outline-offset:-2px}
tr.row-only-b td,tr.row-only-b th{background:var(--del)!important}
tr.row-only-o td,tr.row-only-o th{background:var(--ins)!important}
tr.row-moved td,tr.row-moved th{background:var(--mov)!important}
.mapcol{flex:none;width:14px;display:flex;flex-direction:column;background:#e3e6ea;border-left:1px solid var(--line)}
.mapcol .fname{padding:6px 0}
#map{flex:1;position:relative;cursor:pointer}
#map i{position:absolute;left:2px;right:2px;min-height:2px;border-radius:1px}
#map i.changed{background:#5b84e6}#map i.onlyBase{background:#e06a6a}#map i.onlyOther{background:#46a35e}
#map b{position:absolute;left:0;right:0;background:rgba(47,95,208,.12);border:1px solid rgba(47,95,208,.45);pointer-events:none}
#info{display:none;flex:none;max-height:36vh;overflow:auto;padding:6px 14px 10px;background:var(--paper);border-top:2px solid var(--accent)}
body.info #info{display:block}
#info .ih{display:flex;gap:12px;align-items:baseline;margin-bottom:4px;font-size:12px;color:var(--muted)}
#info .ih b{color:var(--ink);font-size:13px}
table.cl{border-collapse:collapse;width:100%;margin:4px 0;font-size:12px}
table.cl th,table.cl td{border-bottom:1px solid #e5e7eb;padding:2px 6px;text-align:left;vertical-align:top}
table.cl tr[data-c]{cursor:pointer}
table.cl tr[data-c]:hover td{background:#f3f6fd}
table.cl td.d{text-align:right;font-variant-numeric:tabular-nums;white-space:nowrap;font-weight:600}
table.cl tr.f td{color:var(--muted)}
table.cl tr.st td{background:#fafbfc}
body.only .blk.same,body.only .blk.sec.clean{display:none}
@media (max-width:900px){.chips{display:none}.blk{padding:4px 8px}}
"#;

const SCRIPT: &str = r#"
(function(){
  var body=document.body, pb=document.getElementById('pb'), po=document.getElementById('po');
  var B=[].slice.call(pb.children), O=[].slice.call(po.children);
  var map=document.getElementById('map'), info=document.getElementById('info'), counter=document.getElementById('at');
  var changed=B.filter(function(b){return b.hasAttribute('data-i')&&!b.classList.contains('same')});
  var at=-1, view=null;
  if(window.innerWidth<900) body.classList.add('notoc');
  // Each item is one block in each pane; both get the taller block's height.
  function equalize(){
    var i, h=[];
    for(i=0;i<B.length;i++){B[i].style.height='';O[i].style.height=''}
    for(i=0;i<B.length;i++) h.push(Math.max(B[i].offsetHeight,O[i].offsetHeight));
    for(i=0;i<B.length;i++) if(h[i]){B[i].style.height=h[i]+'px';O[i].style.height=h[i]+'px'}
    drawMap();
  }
  // A scroll in either pane moves the other. The position written to the other
  // pane is remembered, so the echo it raises is recognized and not sent back;
  // any other scroll there is the user's and moves this pane in turn.
  function follow(src,dst){
    src.addEventListener('scroll',function(){
      var echo=src.echo; src.echo=null;
      if(echo&&echo[0]===src.scrollTop&&echo[1]===src.scrollLeft){ showView(); return; }
      if(dst.scrollTop!==src.scrollTop||dst.scrollLeft!==src.scrollLeft){
        dst.scrollTop=src.scrollTop; dst.scrollLeft=src.scrollLeft;
        // The other pane may clamp a position it cannot reach.
        dst.echo=[dst.scrollTop,dst.scrollLeft];
      }
      showView();
    },{passive:true});
  }
  follow(pb,po); follow(po,pb);
  function scrollTo(el){ pb.scrollTop=Math.max(0,el.offsetTop-24); }
  function details(el){
    var i=el.getAttribute('data-i'), t=document.getElementById('d'+i);
    info.innerHTML='';
    if(t) info.appendChild(t.content.cloneNode(true));
    else info.textContent=el.classList.contains('onlyBase')?'Only in the base document.':el.classList.contains('onlyOther')?'Only in the other document.':'No cell or wording changes to list for this item.';
  }
  function select(k,scroll){
    if(!changed.length) return;
    at=(k+changed.length)%changed.length;
    [].forEach.call(document.querySelectorAll('.blk.focus'),function(x){x.classList.remove('focus')});
    var el=changed[at], j=B.indexOf(el);
    el.classList.add('focus'); O[j].classList.add('focus');
    if(scroll) scrollTo(el);
    counter.textContent=(at+1)+' / '+changed.length;
    details(el);
  }
  function go(k){ select(k,true); }
  document.getElementById('next').onclick=function(){go(at+1)};
  document.getElementById('prev').onclick=function(){go(at-1)};
  function toggle(button,cls,invert){
    button.onclick=function(){ var on=body.classList.toggle(cls); button.setAttribute('aria-pressed',String(invert?!on:on)); equalize(); };
    button.setAttribute('aria-pressed',String(invert?!body.classList.contains(cls):body.classList.contains(cls)));
  }
  toggle(document.getElementById('tocbtn'),'notoc',true);
  toggle(document.getElementById('infobtn'),'info',false);
  document.getElementById('only').onchange=function(e){ body.classList.toggle('only',e.target.checked); equalize(); };
  [pb,po].forEach(function(pane){
    var list=pane===pb?B:O;
    pane.addEventListener('click',function(e){
      var blk=e.target.closest('.blk'); if(!blk) return;
      var k=changed.indexOf(B[list.indexOf(blk)]); if(k>=0) select(k,false);
    });
  });
  // A change-list row outlines its cell pair in the panes.
  info.addEventListener('click',function(e){
    var row=e.target.closest('tr[data-c]'); if(!row||at<0) return;
    var key='c'+changed[at].getAttribute('data-i')+'-'+row.getAttribute('data-c');
    var cell=document.querySelector('#pb [data-k="'+key+'"]')||document.querySelector('#po [data-k="'+key+'"]');
    hot(key);
    if(cell){
      var pane=cell.closest('.pane'), c=cell.getBoundingClientRect(), r=pane.getBoundingClientRect();
      pane.scrollTop+=c.top-r.top-pane.clientHeight/3;
      if(c.left<r.left||c.right>r.left+pane.clientWidth) pane.scrollLeft+=c.left-r.left-pane.clientWidth/3;
    }
  });
  function hot(key){
    [].forEach.call(document.querySelectorAll('.hot'),function(x){x.classList.remove('hot')});
    if(key) [].forEach.call(document.querySelectorAll('[data-k="'+key+'"]'),function(x){x.classList.add('hot')});
  }
  document.addEventListener('mouseover',function(e){ var c=e.target.closest('[data-k]'); if(c) hot(c.getAttribute('data-k')); });
  [].forEach.call(document.querySelectorAll('#toc a'),function(a){
    a.onclick=function(e){ e.preventDefault(); var el=pb.querySelector('[data-s="'+a.getAttribute('data-s')+'"]'); if(el&&el.offsetParent) scrollTo(el); };
  });
  document.addEventListener('keydown',function(e){
    if(e.target.tagName==='INPUT'||e.ctrlKey||e.metaKey||e.altKey) return;
    if(e.key==='n'||e.key==='j') go(at+1);
    else if(e.key==='p'||e.key==='k') go(at-1);
    else if(e.key==='i') document.getElementById('infobtn').click();
  });
  function drawMap(){
    map.innerHTML=''; var H=pb.scrollHeight||1;
    changed.forEach(function(el,k){
      if(!el.offsetParent) return;
      var m=document.createElement('i');
      m.className=el.classList.contains('onlyBase')?'onlyBase':el.classList.contains('onlyOther')?'onlyOther':'changed';
      m.style.top=(el.offsetTop/H*100)+'%'; m.style.height=(el.offsetHeight/H*100)+'%';
      m.onclick=function(e){e.stopPropagation();go(k)};
      map.appendChild(m);
    });
    view=document.createElement('b'); map.appendChild(view); showView();
  }
  function showView(){ if(!view) return; var H=pb.scrollHeight||1; view.style.top=(pb.scrollTop/H*100)+'%'; view.style.height=(pb.clientHeight/H*100)+'%'; }
  map.onclick=function(e){ pb.scrollTop=e.offsetY/map.clientHeight*pb.scrollHeight-pb.clientHeight/2; };
  var resized=0;
  window.addEventListener('resize',function(){clearTimeout(resized);resized=setTimeout(equalize,150)});
  counter.textContent=changed.length?changed.length+' changes':'no changes';
  equalize();
  if(document.fonts&&document.fonts.ready) document.fonts.ready.then(equalize);
})();
"#;

fn status_class(s: Status) -> &'static str {
    match s {
        Status::Same => "same",
        Status::Changed => "changed",
        Status::OnlyBase => "onlyBase",
        Status::OnlyOther => "onlyOther",
    }
}

/// A self-contained side-by-side report. Each document gets its own pane,
/// rendered at DART's sizes; the panes scroll together with every item aligned,
/// changed cells and, within text, the changed characters highlighted, a change
/// map, a section index, next/previous navigation, and a panel listing the
/// selected item's changes.
/// No external resources are loaded.
pub fn render_html(c: &Comparison, base: &Doc, other: &Doc) -> String {
    let section_name = |s: &str| esc(if s.is_empty() { "(document start)" } else { s });
    let mut sections: Vec<(String, usize)> = Vec::new();
    for item in &c.items {
        if sections.last().is_none_or(|s| s.0 != item.section) {
            sections.push((item.section.clone(), 0));
        }
        if item.status != Status::Same {
            sections.last_mut().expect("pushed").1 += 1;
        }
    }
    let mut out = String::new();
    let s = &c.summary;
    let _ = write!(
        out,
        "<!doctype html><html lang=\"ko\"><head><meta charset=\"utf-8\"><meta name=\"viewport\" content=\"width=device-width,initial-scale=1\"><title>DSD 비교 — {} ↔ {}</title><style>{STYLE}</style></head><body class=\"info\">",
        esc(&c.base.file),
        esc(&c.other.file)
    );
    let _ = write!(
        out,
        "<header class=\"bar\"><h1>DSD 비교</h1><div class=\"chips\"><span>{} changed</span><span>{} only in base</span><span>{} only in other</span><span>{} value · {} text · {} format cells</span><span>{} rows</span></div>\
<div class=\"ctl\"><button id=\"prev\" title=\"p / k\">◀ prev</button><span id=\"at\"></span><button id=\"next\" title=\"n / j\">next ▶</button>\
<label><input type=\"checkbox\" id=\"only\"> changes only</label><button id=\"tocbtn\">sections</button><button id=\"infobtn\" title=\"i\">details</button></div></header>\
<div class=\"main\"><nav id=\"toc\">",
        s.changed, s.only_base, s.only_other, s.value_changes, s.text_cell_changes, s.format_changes, s.row_changes,
    );
    for (k, (name, changed)) in sections.iter().enumerate() {
        let _ = write!(
            out,
            "<a href=\"#\" data-s=\"{k}\" class=\"{}\"><span>{}</span><b>{changed}</b></a>",
            if *changed == 0 { "clean" } else { "" },
            section_name(name)
        );
    }
    out.push_str("</nav>");
    for (base_side, id, side, file) in [(true, "pb", "b", &c.base.file), (false, "po", "o", &c.other.file)] {
        let doc = if base_side { base } else { other };
        let _ = write!(
            out,
            "<div class=\"col {side}\"><div class=\"fname\" title=\"{f}\"><small>{}</small>{f}</div><div class=\"pane\" id=\"{id}\">",
            if base_side { "BASE" } else { "OTHER" },
            f = esc(file)
        );
        let mut current: Option<&str> = None;
        let mut sec_index = 0;
        for (index, item) in c.items.iter().enumerate() {
            if current != Some(item.section.as_str()) {
                let clean = sections.get(sec_index).is_some_and(|s| s.1 == 0);
                let _ = write!(
                    out,
                    "<div class=\"blk sec{}\" data-s=\"{sec_index}\">{}</div>",
                    if clean { " clean" } else { "" },
                    section_name(&item.section)
                );
                current = Some(item.section.as_str());
                sec_index += 1;
            }
            let node = if base_side { item.base_node } else { item.other_node };
            let _ = write!(
                out,
                "<div class=\"blk {}{}\" data-i=\"{index}\">{}</div>",
                status_class(item.status),
                if node.is_none() { " void" } else { "" },
                render_side(doc, node, item, index, base_side)
            );
        }
        out.push_str("</div></div>");
    }
    out.push_str("<div class=\"mapcol\"><div class=\"fname\">&nbsp;</div><div id=\"map\" title=\"change map\"></div></div></div><section id=\"info\">Select a changed block, or press n, to list its changes here.</section>");
    for (index, item) in c.items.iter().enumerate() {
        if let Some(list) = change_list(item) {
            let _ = write!(
                out,
                "<template id=\"d{index}\"><div class=\"ih\"><b>{}</b><span>{} ↔ {}</span></div>{list}</template>",
                section_name(&item.section),
                loc_label(item.base.as_ref()),
                loc_label(item.other.as_ref())
            );
        }
    }
    let _ = write!(out, "<script>{SCRIPT}</script></body></html>");
    out.push('\n');
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn note_headings_skip_subsections() {
        assert_eq!(note_heading("12. 종속기업투자\n(1) 내역"), Some((12, "종속기업투자".into())));
        assert_eq!(note_heading("⏎3.1. 회사가 신규"), None);
        assert_eq!(note_heading("3.1. 회사가 신규"), None);
        assert_eq!(note_heading("(1) 일반"), None);
        assert_eq!(note_heading("24. 법인세비용(수익)"), Some((24, "법인세비용".into())));
    }

    #[test]
    fn keys_ignore_markers_and_company_forms() {
        assert_eq!(match_key("주식회사 가나다테크(*2)"), match_key("㈜가나다테크(*1)"));
        assert_eq!(match_key("합병차손(*1)"), "합병차손");
        assert_ne!(match_key("제14회차"), match_key("제15회차"));
    }

    #[test]
    fn numbers_and_deltas() {
        assert_eq!(delta("1,000", "(500)"), Some("-1,500".into()));
        assert_eq!(delta("-", "1,234,567,890"), Some("+1,234,567,890".into()));
        assert_eq!(delta("12.34%", "20%"), Some("+7.66%p".into()));
        assert_eq!(delta("△1,000", "-1,000"), Some("0".into()));
        assert_eq!(delta("가", "1"), None);
        assert_eq!(cell_change("1,000", "1000").map(|c| c.0), Some(CellKind::Format));
        assert_eq!(cell_change("-", "").map(|c| c.0), Some(CellKind::Format));
        assert_eq!(cell_change("1,000", "1,001").map(|c| c.0), Some(CellKind::Value));
        assert_eq!(cell_change("㈜가나다테크(*1)", "주식회사 가나다테크(*2)").map(|c| c.0), Some(CellKind::Format));
    }

    #[test]
    fn key_pairing_keeps_order_and_fuzzy_matches() {
        let k = |v: &[&str]| v.iter().map(|s| (*s).to_owned()).collect::<Vec<_>>();
        let p = pair_keys(&k(&["a", "현금및현금성자산", "c"]), &k(&["a", "현금및현금성자산(주1)", "x", "c"]));
        assert_eq!(p, vec![(Some(0), Some(0)), (Some(1), Some(1)), (None, Some(2)), (Some(2), Some(3))]);
    }

    #[test]
    fn usermarks_become_validated_css() {
        assert_eq!(
            usermark_css("F-BT12  0X1A2B3C BC0XFFFFFF B A-R"),
            format!(
                "font-family:{BATANG};font-size:12pt;color:#1A2B3C;background-color:#FFFFFF;font-weight:700;text-align:right;"
            )
        );
        assert_eq!(usermark_css("F-10 !B"), "font-size:10pt;font-weight:400;");
        // Malformed values never reach the style attribute.
        assert_eq!(usermark_css("0X12;}x BC0XGGGGGG F-999 F-1 Q"), "");
    }

    #[test]
    fn marks_narrow_to_changed_characters() {
        // An appended note reference or a swapped syllable marks only those characters.
        assert_eq!(render_words("가나재고(주9)", Some("가나재고"), false), "가나재고<ins>(주9)</ins>");
        assert_eq!(render_words("갑동의 방침", Some("을동의 방침"), true), "<del>갑</del>동의 방침");
        assert_eq!(render_words("제 4 기(재작성)", Some("제 4 기"), false), "제 4 기<ins>(재작성)</ins>");
        // A rewrite marks whole words; inserted words are marked whole.
        assert_eq!(render_words("가나다", Some("라마바"), true), "<del>가나다</del>");
        assert_eq!(render_words("가 나 다", Some("가 다"), false), "가 <ins>나</ins> 다");
        assert_eq!(render_words("a<b", Some("a<c"), false), "a&lt;<ins>b</ins>");
    }

    #[test]
    fn words_keep_spacing() {
        assert_eq!(
            words("가 나\n다"),
            vec![("가".into(), " ".into()), ("나".into(), "\n".into()), ("다".into(), String::new())]
        );
    }
}
