//! The workbook model behind `dsd xlsx`: which sheet each block lands on,
//! where every table cell sits, and what each cell holds.
//!
//! Partition, layout, and typing are `.dsd` semantics, so they live here;
//! [`crate::xlsx`] only serializes the model. A table's grid maps one-to-one
//! onto a rectangle of cells, so `T#/R#/C#` coordinates and cell references
//! convert both ways through [`Placement`].

use std::collections::BTreeMap;

use serde::Serialize;

use crate::amount;
use crate::check::Issue;
use crate::compare::{next_note, note_heading};
use crate::doc::{BLOCK_TEXT_TAGS, CR_TOKEN, Doc, Table, is_scaffold};
use crate::error::{Error, Result};
use crate::text::{
    NESTED_TABLE_MARK, block_text_outside_tables, collapse, compact, is_py_space, node_text, node_text_outside_tables,
    unverified_schema, unverified_tags,
};
use crate::xml::{Item, NodeId};

/// The first sheet: one linked row per sheet.
pub const INDEX_SHEET: &str = "총괄표";
/// Excel's worksheet limits.
pub const MAX_ROWS: u32 = 1_048_576;
pub const MAX_COLS: u32 = 16_384;
/// Excel's limit on characters in one cell.
pub const MAX_CELL_CHARS: usize = 32_767;
/// Excel's limit on sheet name length.
pub const MAX_SHEET_NAME: usize = 31;
/// Rows 1-2 hold the sheet title and the link back to the index; row 3 is blank.
pub const FIRST_CONTENT_ROW: u32 = 3;
/// Prefix of the row that names an image's archive member.
pub const IMAGE_MARK: &str = "[이미지]";

/// Statement names a statement title table ends with.
const STATEMENT_NAMES: [&str; 7] =
    ["재무상태표", "손익계산서", "자본변동표", "현금흐름표", "대차대조표", "이익잉여금처분계산서", "결손금처리계산서"];
/// A statement title is short: a name with at most a period or consolidation prefix.
const MAX_STATEMENT_TITLE: usize = 20;
/// Column widths in pixels: DART `COL` widths, clamped to stay readable.
const MIN_COL_PX: u16 = 24;
const MAX_COL_PX: u16 = 360;
const FORBIDDEN_NAME_CHARS: [char; 7] = ['[', ']', ':', '*', '?', '/', '\\'];

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "kebab-case")]
pub enum SheetKind {
    Index,
    Cover,
    Toc,
    Body,
    Section,
    Statement,
    Note,
}

impl SheetKind {
    /// The kind as the index sheet shows it.
    pub const fn label(self) -> &'static str {
        match self {
            Self::Index => "총괄표",
            Self::Cover => "표지",
            Self::Toc => "목차",
            Self::Body => "본문",
            Self::Section => "섹션",
            Self::Statement => "재무제표",
            Self::Note => "주석",
        }
    }
}

#[derive(Debug, Clone, PartialEq)]
pub enum Value {
    Text(String),
    /// A figure and the number format that renders it as its source text.
    Number {
        value: f64,
        format: String,
    },
    /// A link to cell A1 of another sheet (an index into [`Workbook::sheets`]).
    Link {
        sheet: usize,
        text: String,
    },
}

/// What a cell is for; the writer turns it into a style.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum Role {
    #[default]
    Plain,
    /// A sheet's title in row 1.
    Title,
    /// `TITLE` and `COVER-TITLE` lines.
    Bold,
    /// `TH` cells and the index sheet's column headers.
    Header,
    /// The grey `T#` marker beside a table.
    Marker,
}

#[derive(Debug, Clone, PartialEq)]
pub struct Cell {
    /// 0-based row and column.
    pub row: u32,
    pub col: u16,
    /// `None` for an empty cell that only carries a style (a table border).
    pub value: Option<Value>,
    pub role: Role,
    pub border: bool,
    /// The text holds line breaks.
    pub wrap: bool,
}

/// A merged range; its value and style are those of the cell at its top left.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Merge {
    pub first_row: u32,
    pub first_col: u16,
    pub last_row: u32,
    pub last_col: u16,
}

/// Where a table's grid sits: grid cell (R, C) is at (`row` + R - 1, `col` + C - 1).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Placement {
    pub table: usize,
    /// Index into [`Workbook::sheets`].
    pub sheet: usize,
    pub row: u32,
    pub col: u16,
    pub rows: u32,
    pub cols: u16,
}

impl Placement {
    /// The workbook defined name; plain `T13` would collide with a cell address.
    pub fn name(&self) -> String {
        format!("DSD_T{}", self.table)
    }

    /// `A7:F45`.
    pub fn range(&self) -> String {
        let last_row = self.row + self.rows - 1;
        let last_col = self.col + self.cols - 1;
        format!("{}:{}", cell_ref(self.row, self.col, false), cell_ref(last_row, last_col, false))
    }

    /// `$A$7:$F$45`, for defined names.
    pub fn absolute_range(&self) -> String {
        let last_row = self.row + self.rows - 1;
        let last_col = self.col + self.cols - 1;
        format!("{}:{}", cell_ref(self.row, self.col, true), cell_ref(last_row, last_col, true))
    }
}

/// An A1 reference for 0-based coordinates.
pub fn cell_ref(row: u32, col: u16, absolute: bool) -> String {
    let mut letters = Vec::new();
    let mut n = u32::from(col) + 1;
    while n > 0 {
        let rem = (n - 1) % 26;
        letters.push(char::from(b'A' + rem as u8));
        n = (n - 1) / 26;
    }
    let letters: String = letters.into_iter().rev().collect();
    let dollar = if absolute { "$" } else { "" };
    format!("{dollar}{letters}{dollar}{}", row + 1)
}

#[derive(Debug, Clone)]
pub struct Sheet {
    /// The Excel sheet name: sanitized, at most 31 characters, unique.
    pub name: String,
    pub kind: SheetKind,
    /// The full title the index sheet lists.
    pub title: String,
    pub cells: Vec<Cell>,
    pub merges: Vec<Merge>,
    /// Column widths in pixels, by 0-based column.
    pub widths: BTreeMap<u16, u16>,
    /// Rows frozen at the top.
    pub frozen_rows: u32,
    /// Tables placed on this sheet, in order.
    pub tables: Vec<usize>,
    /// Rows in use.
    pub rows: u32,
}

#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct Counts {
    pub sheets: usize,
    pub tables: usize,
    /// Table cells written as numbers.
    pub number_cells: usize,
    /// Non-empty table cells kept as text.
    pub text_cells: usize,
}

#[derive(Debug, Clone)]
pub struct Workbook {
    /// The index sheet first, then content sheets in order of first content.
    pub sheets: Vec<Sheet>,
    /// Every mapped table, in document order.
    pub placements: Vec<Placement>,
    pub counts: Counts,
    pub warnings: Vec<Issue>,
}

/// Build the workbook model for a document.
pub fn workbook(doc: &Doc) -> Result<Workbook> {
    let mut warnings = Vec::new();
    if let Some(schema) = unverified_schema(doc) {
        warnings.push(Issue::warning(
            "unverified-schema",
            format!("schema {schema} is not one this tool was verified on; content is exported as found"),
        ));
    }
    let unverified = unverified_tags(&doc.tree, doc.root());
    if !unverified.is_empty() {
        let names = unverified.iter().map(|t| format!("<{t}>")).collect::<Vec<_>>().join(", ");
        warnings.push(Issue::warning(
            "unverified-element",
            format!("{names} not seen in verified DART output; their content is exported as text"),
        ));
    }
    let mut parts = partition(doc);
    parts.retain(|p| !p.blocks.is_empty());
    let names = sheet_names(&parts, &mut warnings);
    let mut sheets = vec![Sheet {
        name: INDEX_SHEET.to_owned(),
        kind: SheetKind::Index,
        title: INDEX_SHEET.to_owned(),
        cells: Vec::new(),
        merges: Vec::new(),
        widths: BTreeMap::new(),
        frozen_rows: 3,
        tables: Vec::new(),
        rows: 0,
    }];
    let mut placements = Vec::new();
    let mut counts = Counts::default();
    for (part, name) in parts.iter().zip(names) {
        let sheet = sheets.len();
        let mut layout = Layout::new(doc, sheet, name, part, &mut placements, &mut counts, &mut warnings);
        for block in &part.blocks {
            layout.place(block)?;
        }
        sheets.push(layout.finish()?);
    }
    sheets[0] = index_sheet(&sheets);
    counts.sheets = sheets.len();
    counts.tables = placements.len();
    Ok(Workbook { sheets, placements, counts, warnings })
}

// ---------------------------------------------------------------------------
// partition

#[derive(Debug, Clone, PartialEq, Eq)]
enum Block {
    /// Text rows; an empty string is a blank row.
    Lines {
        lines: Vec<String>,
        bold: bool,
    },
    /// An image's archive member name.
    Image(String),
    Table(usize),
}

#[derive(Debug, Clone)]
struct Part {
    kind: SheetKind,
    title: String,
    /// The preferred sheet name before sanitizing.
    name: String,
    blocks: Vec<Block>,
}

/// Where the walk is: inside COVER/TOC (no statement or note sheets), inside
/// a notes region, and the last note number seen there.
#[derive(Debug, Clone, Copy, Default)]
struct Scope {
    plain: bool,
    notes: bool,
    note: Option<u32>,
}

struct Partition<'a> {
    doc: &'a Doc,
    parts: Vec<Part>,
    current: Option<usize>,
    body: Option<usize>,
    scope: Scope,
}

fn partition(doc: &Doc) -> Vec<Part> {
    let mut p = Partition { doc, parts: Vec::new(), current: None, body: None, scope: Scope::default() };
    p.walk(doc.root());
    p.parts
}

/// The compacted title when a table's first non-empty cell names a statement.
fn statement_title(text: &str) -> Option<String> {
    let c = compact(text);
    (c.chars().count() <= MAX_STATEMENT_TITLE && STATEMENT_NAMES.iter().any(|n| c.ends_with(n))).then_some(c)
}

fn first_cell_text(doc: &Doc, table: &Table) -> String {
    table.cells.iter().map(|c| node_text(&doc.tree, c.node, false)).find(|t| !compact(t).is_empty()).unwrap_or_default()
}

/// Loose text in a container, split into lines as block text is.
fn loose_lines(text: &str) -> Vec<String> {
    let text = text.replace(['\r', '\n'], "").replace(CR_TOKEN, "\n");
    let mut lines: Vec<String> = text.split('\n').map(|l| l.trim_end_matches(is_py_space).to_owned()).collect();
    while lines.first().is_some_and(|l| l.trim_matches(is_py_space).is_empty()) {
        lines.remove(0);
    }
    while lines.last().is_some_and(|l| l.trim_matches(is_py_space).is_empty()) {
        lines.pop();
    }
    lines
}

impl Partition<'_> {
    fn open(&mut self, kind: SheetKind, title: String, name: String) {
        self.parts.push(Part { kind, title, name, blocks: Vec::new() });
        self.current = Some(self.parts.len() - 1);
    }

    fn push(&mut self, block: Block) {
        let target = if let Some(i) = self.current {
            i
        } else {
            // Content outside every region shares one sheet.
            let i = *self.body.get_or_insert_with(|| {
                self.parts.push(Part {
                    kind: SheetKind::Body,
                    title: "본문".to_owned(),
                    name: "본문".to_owned(),
                    blocks: Vec::new(),
                });
                self.parts.len() - 1
            });
            self.current = Some(i);
            i
        };
        self.parts[target].blocks.push(block);
    }

    fn walk(&mut self, node: NodeId) {
        let tree = &self.doc.tree;
        for item in &tree.node(node).items {
            match item {
                Item::Text(t) => {
                    let lines = loose_lines(t);
                    if !lines.is_empty() {
                        self.push(Block::Lines { lines, bold: false });
                    }
                }
                Item::Node(child) => self.visit(*child),
            }
        }
    }

    fn visit(&mut self, node: NodeId) {
        let doc = self.doc;
        let tree = &doc.tree;
        let tag = tree.tag(node);
        if is_scaffold(tag) || tag == "DOCUMENT-HEADER" {
            return;
        }
        if let Some(table) = doc.table_for_node(node) {
            if !self.scope.plain
                && let Some(title) = statement_title(&first_cell_text(doc, table))
                && !self.repeats_statement_title(&title)
            {
                self.open(SheetKind::Statement, title.clone(), title);
            }
            self.push(Block::Table(table.index));
            self.nested_tables(node);
        } else if tag == "IMG" {
            self.push(Block::Image(node_text(tree, node, false)));
        } else if BLOCK_TEXT_TAGS.contains(&tag) {
            self.text_block(node, tag);
        } else if tag == "COVER" || tag == "TOC" {
            let (kind, title) = if tag == "COVER" { (SheetKind::Cover, "표지") } else { (SheetKind::Toc, "목차") };
            self.region(
                node,
                kind,
                title.to_owned(),
                title.to_owned(),
                Scope { plain: true, notes: false, note: None },
            );
        } else if let Some(title) = (tag == "PART" || tag == "SECTION-1" || tag == "SECTION-2")
            .then(|| tree.child_tag(node, "TITLE"))
            .flatten()
            .map(|t| collapse(&node_text(tree, t, false)))
            .filter(|t| !t.is_empty())
        {
            let (kind, name) = match statement_title(&title) {
                Some(compacted) => (SheetKind::Statement, compacted),
                None => (SheetKind::Section, title.clone()),
            };
            let notes = compact(&title).contains("주석");
            // A nested section keeps the note numbering it sits in.
            let note = if tag == "SECTION-2" { self.scope.note } else { None };
            let scope = Scope { plain: self.scope.plain, notes, note };
            self.region(node, kind, title, name, scope);
        } else {
            self.walk(node);
        }
    }

    /// The current sheet already is this statement and holds no table yet:
    /// a statement section whose first table repeats its title.
    fn repeats_statement_title(&self, title: &str) -> bool {
        self.current.map(|i| &self.parts[i]).is_some_and(|p| {
            p.kind == SheetKind::Statement && p.name == title && !p.blocks.iter().any(|b| matches!(b, Block::Table(_)))
        })
    }

    /// A region opens its own sheet; afterwards the parent's sheet continues.
    fn region(&mut self, node: NodeId, kind: SheetKind, title: String, name: String, scope: Scope) {
        let saved = (self.current, self.scope);
        self.open(kind, title, name);
        self.scope = scope;
        self.walk(node);
        (self.current, self.scope) = saved;
    }

    /// Tables inside a table cell or a paragraph follow it as tables of their own.
    fn nested_tables(&mut self, node: NodeId) {
        let doc = self.doc;
        for n in doc.tree.iter_tag(node, "TABLE").into_iter().filter(|&n| n != node) {
            if let Some(t) = doc.table_for_node(n) {
                self.push(Block::Table(t.index));
            }
        }
    }

    fn text_block(&mut self, node: NodeId, tag: &str) {
        let text = block_text_outside_tables(&self.doc.tree, node);
        let squeezed = compact(&text);
        if squeezed.is_empty() {
            return;
        }
        if squeezed != NESTED_TABLE_MARK {
            let bold = tag == "TITLE" || tag == "COVER-TITLE";
            let lines: Vec<String> = text.split('\n').map(str::to_owned).collect();
            let mut start = 0;
            if tag == "P" && self.scope.notes && !self.scope.plain {
                // A note heading opens a sheet, even after a line break.
                for (i, line) in lines.iter().enumerate() {
                    let Some((n, _)) = note_heading(line) else { continue };
                    if !next_note(self.scope.note, n) {
                        continue;
                    }
                    if i > start {
                        self.push(Block::Lines { lines: lines[start..i].to_vec(), bold });
                    }
                    self.scope.note = Some(n);
                    self.open(SheetKind::Note, collapse(line), format!("주석{n}"));
                    start = i;
                }
            }
            self.push(Block::Lines { lines: lines[start..].to_vec(), bold });
        }
        self.nested_tables(node);
    }
}

// ---------------------------------------------------------------------------
// names

fn utf16_len(s: &str) -> usize {
    s.encode_utf16().count()
}

/// The longest prefix of `s` within `max` UTF-16 units.
fn truncate_utf16(s: &str, max: usize) -> &str {
    let mut used = 0;
    for (i, c) in s.char_indices() {
        used += c.len_utf16();
        if used > max {
            return &s[..i];
        }
    }
    s
}

/// A valid Excel sheet name: no `[]:*?/\` or control characters, no leading
/// or trailing apostrophe, at most 31 characters.
pub fn sanitize_sheet_name(name: &str) -> String {
    let cleaned: String = name
        .chars()
        .filter(|c| !c.is_control())
        .map(|c| if FORBIDDEN_NAME_CHARS.contains(&c) { '_' } else { c })
        .collect();
    let trimmed = cleaned.trim().trim_matches('\'').trim();
    let short = truncate_utf16(trimmed, MAX_SHEET_NAME).trim_end().trim_end_matches('\'');
    if short.is_empty() { "시트".to_owned() } else { short.to_owned() }
}

/// Unique sheet names, case-insensitively as Excel compares them; the index
/// sheet's name is taken first. Changed names are reported.
fn sheet_names(parts: &[Part], warnings: &mut Vec<Issue>) -> Vec<String> {
    let mut used: Vec<String> = vec![INDEX_SHEET.to_lowercase(), "history".to_owned()];
    let mut names = Vec::with_capacity(parts.len());
    for part in parts {
        let base = sanitize_sheet_name(&part.name);
        let mut name = base.clone();
        let mut k = 2;
        while used.contains(&name.to_lowercase()) {
            let suffix = format!(" ({k})");
            let room = MAX_SHEET_NAME - utf16_len(&suffix);
            name = format!("{}{suffix}", truncate_utf16(&base, room).trim_end());
            k += 1;
        }
        used.push(name.to_lowercase());
        if name != part.name {
            warnings.push(Issue::warning(
                "sheet-name-changed",
                format!(
                    "{} sheet {:?} is named {name:?} (Excel names are unique and at most 31 characters)",
                    part.kind.label(),
                    part.name
                ),
            ));
        }
        names.push(name);
    }
    names
}

// ---------------------------------------------------------------------------
// layout

struct Layout<'a> {
    doc: &'a Doc,
    sheet: Sheet,
    index: usize,
    row: u32,
    after_table: bool,
    placements: &'a mut Vec<Placement>,
    counts: &'a mut Counts,
    warnings: &'a mut Vec<Issue>,
}

impl<'a> Layout<'a> {
    fn new(
        doc: &'a Doc,
        index: usize,
        name: String,
        part: &Part,
        placements: &'a mut Vec<Placement>,
        counts: &'a mut Counts,
        warnings: &'a mut Vec<Issue>,
    ) -> Self {
        let mut sheet = Sheet {
            name,
            kind: part.kind,
            title: part.title.clone(),
            cells: Vec::new(),
            merges: Vec::new(),
            widths: BTreeMap::new(),
            frozen_rows: 2,
            tables: Vec::new(),
            rows: 0,
        };
        sheet.cells.push(text_cell(0, 0, part.title.clone(), Role::Title));
        sheet.cells.push(Cell {
            row: 1,
            col: 0,
            value: Some(Value::Link { sheet: 0, text: INDEX_SHEET.to_owned() }),
            role: Role::Plain,
            border: false,
            wrap: false,
        });
        Self { doc, sheet, index, row: FIRST_CONTENT_ROW, after_table: false, placements, counts, warnings }
    }

    fn place(&mut self, block: &Block) -> Result<()> {
        match block {
            Block::Lines { lines, bold } => {
                self.gap();
                let role = if *bold { Role::Bold } else { Role::Plain };
                for line in lines {
                    if !line.trim_matches(is_py_space).is_empty() {
                        check_length(line, || format!("a text line on sheet {:?}", self.sheet.name))?;
                        self.sheet.cells.push(text_cell(self.row, 0, line.clone(), role));
                    }
                    self.row += 1;
                }
            }
            Block::Image(name) => {
                self.gap();
                self.sheet.cells.push(text_cell(self.row, 0, format!("{IMAGE_MARK} {name}"), Role::Plain));
                self.row += 1;
            }
            Block::Table(index) => self.table(*index)?,
        }
        Ok(())
    }

    /// One blank row after a table.
    fn gap(&mut self) {
        if self.after_table {
            self.row += 1;
            self.after_table = false;
        }
    }

    fn table(&mut self, index: usize) -> Result<()> {
        let doc = self.doc;
        let table = &doc.tables[index - 1];
        // One blank row before a table, unless it opens the sheet.
        if self.row > FIRST_CONTENT_ROW {
            self.row += 1;
        }
        self.after_table = true;
        let (nrows, ncols) = (table.rows.len(), table.ncols());
        if table.oversized || nrows == 0 || ncols == 0 {
            let reason = if table.oversized { "too large to map" } else { "has no grid" };
            self.sheet.cells.push(text_cell(
                self.row,
                0,
                format!("[T{index}] {nrows}x{ncols} table {reason}"),
                Role::Marker,
            ));
            self.warnings.push(Issue::warning(
                "table-not-mapped",
                format!(
                    "T{index} ({nrows}x{ncols}) is {reason}; sheet {:?} names it on one line instead",
                    self.sheet.name
                ),
            ));
            self.row += 1;
            return Ok(());
        }
        // The grid plus the marker column must fit Excel's columns.
        if ncols + 1 > MAX_COLS as usize {
            return Err(Error::invalid(format!("T{index} has {ncols} columns; Excel sheets hold {MAX_COLS}")));
        }
        if !table.issues.is_empty() {
            self.warnings.push(Issue::warning(
                "table-grid-problem",
                format!(
                    "T{index} on sheet {:?} has {} grid problem(s) (see `dsd check`); cells outside its grid are not exported",
                    self.sheet.name,
                    table.issues.len()
                ),
            ));
        }
        let (top, rows, cols) = (self.row, nrows as u32, ncols as u16);
        let border = doc.attr(table.node, "BORDER").is_some_and(|b| b.trim() == "1");
        let tree = &doc.tree;
        for ri in 0..nrows {
            for ci in 0..ncols {
                let (row, col) = (top + ri as u32, ci as u16);
                let owner = table.grid[ri][ci];
                let Some(cell) = owner.map(|i| &table.cells[i]) else {
                    if border {
                        self.sheet.cells.push(Cell { row, col, value: None, role: Role::Plain, border, wrap: false });
                    }
                    continue;
                };
                let is_origin = (cell.row, cell.col) == (ri + 1, ci + 1);
                let role = if tree.tag(cell.node) == "TH" { Role::Header } else { Role::Plain };
                if !is_origin {
                    // Covered by a merge, or by an overlapping cell when no merge was possible.
                    if border && merge_of(table, cell.row - 1, cell.col - 1, top).is_none() {
                        self.sheet.cells.push(Cell { row, col, value: None, role, border, wrap: false });
                    }
                    continue;
                }
                let text = node_text_outside_tables(tree, cell.node);
                check_length(&text, || format!("T{index} R{}C{}", cell.row, cell.col))?;
                let wrap = text.contains('\n');
                let value = if text.is_empty() {
                    None
                } else if let Some(a) = amount::parse(&text) {
                    self.counts.number_cells += 1;
                    Some(Value::Number { value: a.value, format: a.format })
                } else {
                    self.counts.text_cells += 1;
                    Some(Value::Text(text))
                };
                if let Some(merge) = merge_of(table, ri, ci, top) {
                    self.sheet.merges.push(merge);
                }
                if value.is_some() || border || role == Role::Header {
                    self.sheet.cells.push(Cell { row, col, value, role, border, wrap });
                }
            }
        }
        self.sheet.cells.push(text_cell(top, cols, format!("T{index}"), Role::Marker));
        for (ci, &w) in table.cols.iter().enumerate() {
            if let Ok(px) = u16::try_from(w)
                && px > 0
            {
                let px = px.clamp(MIN_COL_PX, MAX_COL_PX);
                let entry = self.sheet.widths.entry(ci as u16).or_insert(px);
                *entry = (*entry).max(px);
            }
        }
        self.placements.push(Placement { table: index, sheet: self.index, row: top, col: 0, rows, cols });
        self.sheet.tables.push(index);
        self.row = top + rows;
        Ok(())
    }

    fn finish(mut self) -> Result<Sheet> {
        if self.row > MAX_ROWS {
            return Err(Error::invalid(format!(
                "sheet {:?} needs {} rows; Excel sheets hold {MAX_ROWS}",
                self.sheet.name, self.row
            )));
        }
        let cells = self.sheet.cells.iter().map(|c| c.row + 1);
        let merges = self.sheet.merges.iter().map(|m| m.last_row + 1);
        let tables = self.placements.iter().filter(|p| p.sheet == self.index).map(|p| p.row + p.rows);
        self.sheet.rows = cells.chain(merges).chain(tables).max().unwrap_or(0);
        Ok(self.sheet)
    }
}

/// The merged range of the cell at grid (ri, ci), when it spans and owns
/// every grid position of its span (overlapping cells cannot merge).
fn merge_of(table: &Table, ri: usize, ci: usize, top: u32) -> Option<Merge> {
    let owner = table.grid[ri][ci]?;
    let cell = &table.cells[owner];
    let last_ri = (ri + cell.rowspan).min(table.rows.len()) - 1;
    let last_ci = (ci + cell.colspan).min(table.ncols()) - 1;
    if last_ri == ri && last_ci == ci {
        return None;
    }
    let owns_all = (ri..=last_ri).all(|r| (ci..=last_ci).all(|c| table.grid[r][c] == Some(owner)));
    owns_all.then(|| Merge {
        first_row: top + ri as u32,
        first_col: ci as u16,
        last_row: top + last_ri as u32,
        last_col: last_ci as u16,
    })
}

fn text_cell(row: u32, col: u16, text: String, role: Role) -> Cell {
    let wrap = text.contains('\n');
    Cell { row, col, value: Some(Value::Text(text)), role, border: false, wrap }
}

fn check_length(text: &str, place: impl FnOnce() -> String) -> Result<()> {
    let n = text.chars().count();
    if n > MAX_CELL_CHARS {
        return Err(Error::invalid(format!("{} holds {n} characters; an Excel cell holds {MAX_CELL_CHARS}", place())));
    }
    Ok(())
}

fn index_sheet(sheets: &[Sheet]) -> Sheet {
    let mut cells = vec![text_cell(0, 0, INDEX_SHEET.to_owned(), Role::Title)];
    for (col, label) in ["시트", "구분", "제목"].into_iter().enumerate() {
        cells.push(text_cell(2, col as u16, label.to_owned(), Role::Header));
    }
    for (i, sheet) in sheets.iter().enumerate().skip(1) {
        let row = FIRST_CONTENT_ROW + i as u32 - 1;
        cells.push(Cell {
            row,
            col: 0,
            value: Some(Value::Link { sheet: i, text: sheet.name.clone() }),
            role: Role::Plain,
            border: false,
            wrap: false,
        });
        cells.push(text_cell(row, 1, sheet.kind.label().to_owned(), Role::Plain));
        cells.push(text_cell(row, 2, sheet.title.clone(), Role::Plain));
    }
    let rows = cells.iter().map(|c| c.row + 1).max().unwrap_or(0);
    Sheet {
        name: INDEX_SHEET.to_owned(),
        kind: SheetKind::Index,
        title: INDEX_SHEET.to_owned(),
        cells,
        merges: Vec::new(),
        widths: BTreeMap::from([(0, 200), (1, 72), (2, 360)]),
        frozen_rows: 3,
        tables: Vec::new(),
        rows,
    }
}

#[cfg(test)]
mod tests {
    use std::fmt::Write as _;

    use super::*;

    fn book(body: &str) -> Workbook {
        let xml = format!(
            "<?xml version=\"1.0\" encoding=\"utf-8\"?>\n<DOCUMENT xmlns:xsi=\"http://www.w3.org/2001/XMLSchema-instance\" xsi:noNamespaceSchemaLocation=\"dart4.xsd\"><BODY>{body}</BODY></DOCUMENT>"
        );
        workbook(&Doc::parse(xml.into_bytes()).unwrap()).unwrap()
    }

    fn sheets(wb: &Workbook) -> Vec<(&str, SheetKind)> {
        wb.sheets.iter().skip(1).map(|s| (s.name.as_str(), s.kind)).collect()
    }

    /// Values in rows 4 and below, in reading order.
    fn lines(sheet: &Sheet) -> Vec<String> {
        let mut cells: Vec<&Cell> =
            sheet.cells.iter().filter(|c| c.row >= FIRST_CONTENT_ROW && c.role != Role::Marker).collect();
        cells.sort_by_key(|c| (c.row, c.col));
        cells
            .into_iter()
            .filter_map(|c| match &c.value {
                Some(Value::Text(t)) => Some(t.clone()),
                Some(Value::Number { value, .. }) => Some(value.to_string()),
                _ => None,
            })
            .collect()
    }

    fn sheet<'a>(wb: &'a Workbook, name: &str) -> &'a Sheet {
        wb.sheets.iter().find(|s| s.name == name).unwrap_or_else(|| panic!("no sheet {name}"))
    }

    fn table(first: &str, rows: &[&str]) -> String {
        let mut t = String::from("<TABLE BORDER=\"1\"><COLGROUP><COL WIDTH=\"100\"/></COLGROUP><TBODY>");
        for cell in std::iter::once(&first).chain(rows) {
            let _ = write!(t, "<TR><TD>{cell}</TD></TR>");
        }
        t.push_str("</TBODY></TABLE>");
        t
    }

    fn section(tag: &str, title: &str, inner: &str) -> String {
        format!("<{tag}><TITLE>{title}</TITLE>{inner}</{tag}>")
    }

    #[test]
    fn statement_titles_are_short_and_end_with_a_statement_name() {
        for title in
            ["재 무 상 태 표", "반기연결포괄손익계산서", "연결 현금흐름표", "이익잉여금처분계산서", "자본변동표"]
        {
            assert!(statement_title(title).is_some(), "{title}");
        }
        assert_eq!(statement_title("재 무 상 태 표").as_deref(), Some("재무상태표"));
        for text in ["과 목", "재무상태표 (계속)", "요약 재무상태표에 관한 설명입니다 그리고 재무상태표", ""]
        {
            assert!(statement_title(text).is_none(), "{text}");
        }
    }

    #[test]
    fn statement_title_tables_open_sheets_until_the_region_ends() {
        let wb = book(&format!(
            "{}{}",
            section(
                "SECTION-1",
                "(첨부)재 무 제 표",
                &format!(
                    "<P>앞말</P>{}{}<P>뒷말</P>{}{}",
                    table("재 무 상 태 표", &["제 5 기"]),
                    table("과 목", &["자산"]),
                    table("포괄손익계산서", &[]),
                    table("과 목", &["매출"]),
                ),
            ),
            section("SECTION-1", "감사의 개요", "<P>끝</P>"),
        ));
        assert_eq!(
            sheets(&wb),
            [
                ("(첨부)재 무 제 표", SheetKind::Section),
                ("재무상태표", SheetKind::Statement),
                ("포괄손익계산서", SheetKind::Statement),
                ("감사의 개요", SheetKind::Section),
            ]
        );
        assert_eq!(lines(sheet(&wb, "(첨부)재 무 제 표")), ["(첨부)재 무 제 표", "앞말"]);
        assert_eq!(lines(sheet(&wb, "재무상태표")), ["재 무 상 태 표", "제 5 기", "과 목", "자산", "뒷말"]);
        assert_eq!(sheet(&wb, "재무상태표").tables, [1, 2]);
        assert_eq!(lines(sheet(&wb, "감사의 개요")), ["감사의 개요", "끝"]);
    }

    #[test]
    fn a_statement_section_with_its_title_table_keeps_one_sheet() {
        let wb = book(&section(
            "SECTION-2",
            "재 무 상 태 표",
            &format!("{}{}{}", table("재 무 상 태 표", &[]), table("과 목", &[]), table("재무상태표", &[])),
        ));
        assert_eq!(sheets(&wb), [("재무상태표", SheetKind::Statement), ("재무상태표 (2)", SheetKind::Statement)]);
        assert_eq!(wb.sheets[1].title, "재 무 상 태 표");
        assert_eq!(wb.sheets[1].tables, [1, 2], "a later title table still opens a sheet");
    }

    #[test]
    fn cells_outside_the_grid_are_reported() {
        let wb = book("<TABLE><COLGROUP><COL/></COLGROUP><TBODY><TR><TD>a</TD><TD>b</TD></TR></TBODY></TABLE>");
        assert!(wb.warnings.iter().any(|w| w.code == "table-grid-problem"), "{:?}", wb.warnings);
    }

    #[test]
    fn rows_count_rows_covered_by_a_span() {
        let wb = book(
            "<TABLE><COLGROUP><COL/><COL/></COLGROUP><TBODY><TR><TD ROWSPAN=\"2\">a</TD><TD>b</TD></TR><TR><TD></TD></TR></TBODY></TABLE>",
        );
        let p = wb.placements[0];
        assert_eq!(wb.sheets[1].rows, p.row + p.rows);
    }

    #[test]
    fn cover_and_toc_never_open_statement_sheets() {
        let wb = book(&format!("<COVER>{}</COVER><TOC>{}</TOC>", table("재무상태표", &[]), table("손익계산서", &[])));
        assert_eq!(sheets(&wb), [("표지", SheetKind::Cover), ("목차", SheetKind::Toc)]);
    }

    #[test]
    fn notes_split_only_inside_a_notes_region() {
        let notes = section(
            "SECTION-2",
            "주석",
            &format!(
                "{}<P>1. 일반사항</P><P>회사는 설립되었습니다.</P><P>2. 회계정책</P><P>(1) 기준</P><P>1. 목록 항목</P><P>9. 너무 먼 번호</P>{}",
                table("(단위: 원)", &[]),
                table("구 분", &["금액"]),
            ),
        );
        let other = section("SECTION-2", "재무상태표", "<P>1. 일반사항</P><P>2. 회계정책</P>");
        let wb = book(&format!("{other}{notes}"));
        assert_eq!(
            sheets(&wb),
            [
                ("재무상태표", SheetKind::Statement),
                ("주석", SheetKind::Section),
                ("주석1", SheetKind::Note),
                ("주석2", SheetKind::Note),
            ]
        );
        assert_eq!(lines(sheet(&wb, "재무상태표")), ["재무상태표", "1. 일반사항", "2. 회계정책"]);
        assert_eq!(lines(sheet(&wb, "주석")), ["주석", "(단위: 원)"]);
        assert_eq!(lines(sheet(&wb, "주석1")), ["1. 일반사항", "회사는 설립되었습니다."]);
        assert_eq!(
            lines(sheet(&wb, "주석2")),
            ["2. 회계정책", "(1) 기준", "1. 목록 항목", "9. 너무 먼 번호", "구 분", "금액"]
        );
        assert_eq!(sheet(&wb, "주석2").title, "2. 회계정책");
    }

    #[test]
    fn table_cells_are_typed_by_display_fidelity() {
        let wb = book(&table("1,234", &["(500)", "5,6,32", "2025", "-", "12.5%", "1,234 (*1)", "　"]));
        let values: Vec<Option<Value>> = wb.sheets[1]
            .cells
            .iter()
            .filter(|c| c.row >= FIRST_CONTENT_ROW && c.role != Role::Marker)
            .map(|c| c.value.clone())
            .collect();
        let number = |value: f64, format: &str| Some(Value::Number { value, format: format.into() });
        assert_eq!(
            values,
            [
                number(1234.0, "#,##0"),
                number(-500.0, "0;(0)"),
                Some(Value::Text("5,6,32".into())),
                number(2025.0, "0"),
                number(0.0, "#,##0;-#,##0;\"-\""),
                number(0.125, "0.0%"),
                Some(Value::Text("1,234 (*1)".into())),
                None,
            ]
        );
        assert_eq!((wb.counts.number_cells, wb.counts.text_cells), (5, 2));
    }

    #[test]
    fn line_breaks_and_spaces_at_span_edges_are_kept() {
        let wb =
            book("<P>기타사항 <SPAN>&amp;cr;</SPAN><SPAN>둘째 줄</SPAN></P><P><SPAN>가 </SPAN><SPAN>나</SPAN></P>");
        assert_eq!(lines(&wb.sheets[1]), ["기타사항", "둘째 줄", "가 나"]);
    }

    #[test]
    fn a_heading_after_a_line_break_opens_its_own_sheet() {
        let wb = book(&section(
            "SECTION-2",
            "주석",
            "<P>1. 일반사항</P><P>마지막 문장입니다.&amp;cr;2. 리스&amp;cr;(1) 사용권자산&amp;cr;3. 차입금</P>",
        ));
        assert_eq!(sheets(&wb).iter().map(|s| s.0).collect::<Vec<_>>(), ["주석", "주석1", "주석2", "주석3"]);
        assert_eq!(lines(sheet(&wb, "주석1")), ["1. 일반사항", "마지막 문장입니다."]);
        assert_eq!(lines(sheet(&wb, "주석2")), ["2. 리스", "(1) 사용권자산"]);
        assert_eq!(lines(sheet(&wb, "주석3")), ["3. 차입금"]);
    }

    #[test]
    fn a_nested_section_returns_to_its_parents_sheet() {
        let wb = book(&format!(
            "<P>머리말</P><COVER><P>표지 글</P></COVER><P>사이</P>{}",
            section("SECTION-1", "가", &format!("<P>하나</P>{}<P>셋</P>", section("SECTION-2", "나", "<P>둘</P>"))),
        ));
        assert_eq!(
            sheets(&wb),
            [
                ("본문", SheetKind::Body),
                ("표지", SheetKind::Cover),
                ("가", SheetKind::Section),
                ("나", SheetKind::Section)
            ]
        );
        assert_eq!(lines(sheet(&wb, "본문")), ["머리말", "사이"]);
        assert_eq!(lines(sheet(&wb, "가")), ["가", "하나", "셋"]);
        assert_eq!(lines(sheet(&wb, "나")), ["나", "둘"]);
    }

    #[test]
    fn sheet_names_are_valid_unique_and_reported_when_changed() {
        let long = "가".repeat(40);
        let wb = book(&format!(
            "{}{}{}{}{}",
            section("SECTION-1", "회사의 개요", "<P>a</P>"),
            section("SECTION-1", "회사의 개요", "<P>b</P>"),
            section("SECTION-1", "총괄표", "<P>c</P>"),
            section("SECTION-1", "가/나: [다]?", "<P>d</P>"),
            section("SECTION-1", &long, "<P>e</P>"),
        ));
        let names: Vec<&str> = sheets(&wb).iter().map(|s| s.0).collect();
        let truncated = "가".repeat(31);
        assert_eq!(names, ["회사의 개요", "회사의 개요 (2)", "총괄표 (2)", "가_나_ _다__", truncated.as_str()]);
        assert_eq!(sheet(&wb, &truncated).title, long, "the index keeps the full title");
        let changed = wb.warnings.iter().filter(|w| w.code == "sheet-name-changed").count();
        assert_eq!(changed, 4);
        assert_eq!(sanitize_sheet_name("'인용'"), "인용");
        assert_eq!(sanitize_sheet_name("::"), "__");
        assert_eq!(sanitize_sheet_name("   "), "시트");
        let mut warnings = Vec::new();
        let part =
            |name: &str| Part { kind: SheetKind::Section, title: name.into(), name: name.into(), blocks: Vec::new() };
        let names = sheet_names(&[part("History"), part("abc"), part("ABC")], &mut warnings);
        assert_eq!(names, ["History (2)", "abc", "ABC (2)"]);
        let again = sheet_names(&[part(&"나".repeat(31)), part(&"나".repeat(31))], &mut warnings);
        assert_eq!(again[1], format!("{} (2)", "나".repeat(27)));
        assert_eq!(again[1].chars().count(), 31);
    }

    #[test]
    fn tables_map_their_grid_with_merges_markers_and_gaps() {
        let wb = book(&section(
            "SECTION-1",
            "표",
            "<P>앞</P><TABLE BORDER=\"1\"><COLGROUP><COL WIDTH=\"500\"/><COL WIDTH=\"10\"/></COLGROUP><THEAD><TR><TH COLSPAN=\"2\">머리</TH></TR></THEAD><TBODY><TR><TD ROWSPAN=\"2\">가</TD><TD>x</TD></TR><TR><TD>  y</TD></TR></TBODY></TABLE><P>뒤</P>",
        ));
        let s = sheet(&wb, "표");
        let p = wb.placements[0];
        assert_eq!((p.table, p.sheet, p.row, p.col, p.rows, p.cols), (1, 1, 6, 0, 3, 2));
        assert_eq!((p.name(), p.range(), p.absolute_range()), ("DSD_T1".into(), "A7:B9".into(), "$A$7:$B$9".into()));
        let at = |r: u32, c: u16| s.cells.iter().find(|x| (x.row, x.col) == (r, c)).unwrap();
        assert_eq!(at(0, 0).role, Role::Title);
        assert!(matches!(at(1, 0).value, Some(Value::Link { sheet: 0, .. })));
        assert_eq!(at(3, 0).value, Some(Value::Text("표".into())));
        assert_eq!(at(4, 0).value, Some(Value::Text("앞".into())));
        assert_eq!(at(6, 0).role, Role::Header);
        assert_eq!(at(6, 2).value, Some(Value::Text("T1".into())));
        assert_eq!(at(6, 2).role, Role::Marker);
        assert_eq!(at(8, 1).value, Some(Value::Text("  y".into())), "leading spaces are kept");
        assert_eq!(at(10, 0).value, Some(Value::Text("뒤".into())), "one blank row after the table");
        assert_eq!(
            s.merges,
            [
                Merge { first_row: 6, first_col: 0, last_row: 6, last_col: 1 },
                Merge { first_row: 7, first_col: 0, last_row: 8, last_col: 0 }
            ]
        );
        assert!(s.cells.iter().all(|c| (c.row, c.col) != (8, 0)), "merged cells are written once");
        assert_eq!(s.widths, BTreeMap::from([(0, MAX_COL_PX), (1, MIN_COL_PX)]));
        assert_eq!(s.rows, 11);
    }

    #[test]
    fn nested_tables_follow_their_container() {
        let inner = table("안쪽", &[]);
        let wb = book(&section(
            "SECTION-1",
            "중첩",
            &format!(
                "<TABLE><COLGROUP><COL/><COL/></COLGROUP><TBODY><TR><TD>바깥</TD><TD><P>위</P>{inner}</TD></TR></TBODY></TABLE><P>{}</P>",
                table("문단 속", &[])
            ),
        ));
        let s = sheet(&wb, "중첩");
        assert_eq!(s.tables, [1, 2, 3]);
        assert_eq!(lines(s), ["중첩", "바깥", &format!("위\n{NESTED_TABLE_MARK}"), "안쪽", "문단 속"]);
        let container = s.cells.iter().find(|c| (c.row, c.col) == (wb.placements[0].row, 1)).unwrap();
        assert!(container.wrap);
        let rows: Vec<u32> = wb.placements.iter().map(|p| p.row).collect();
        assert_eq!(rows, [5, 7, 9], "each table is separated by one blank row");
    }

    #[test]
    fn images_and_unmapped_tables_get_a_line() {
        let wb =
            book("<IMAGE><IMG>그림.jpg</IMG><IMG-CAPTION>설명</IMG-CAPTION></IMAGE><TABLE><TBODY></TBODY></TABLE>");
        let s = &wb.sheets[1];
        assert_eq!(lines(s), [format!("{IMAGE_MARK} 그림.jpg"), "설명".into()]);
        assert!(s.cells.iter().any(|c| c.value == Some(Value::Text("[T1] 0x0 table has no grid".into()))));
        assert!(wb.placements.is_empty());
        assert!(wb.warnings.iter().any(|w| w.code == "table-not-mapped"));
    }

    #[test]
    fn index_sheet_links_every_sheet() {
        let wb = book(&format!("<COVER><P>x</P></COVER>{}", section("SECTION-1", "감사보고서", "<P>y</P>")));
        let index = &wb.sheets[0];
        assert_eq!((index.name.as_str(), index.kind, index.frozen_rows), (INDEX_SHEET, SheetKind::Index, 3));
        let links: Vec<(u32, usize)> = index
            .cells
            .iter()
            .filter_map(|c| match &c.value {
                Some(Value::Link { sheet, .. }) => Some((c.row, *sheet)),
                _ => None,
            })
            .collect();
        assert_eq!(links, [(3, 1), (4, 2)]);
        assert_eq!(wb.counts.sheets, 3);
    }

    #[test]
    fn cells_beyond_excel_limits_fail() {
        let xml = format!(
            "<?xml version=\"1.0\"?><DOCUMENT><BODY><P>{}</P></BODY></DOCUMENT>",
            "가".repeat(MAX_CELL_CHARS + 1)
        );
        let err = workbook(&Doc::parse(xml.into_bytes()).unwrap()).unwrap_err();
        assert_eq!(err.code, crate::ErrorCode::InvalidArgument);
    }

    #[test]
    fn cell_references() {
        assert_eq!(cell_ref(0, 0, false), "A1");
        assert_eq!(cell_ref(9, 25, false), "Z10");
        assert_eq!(cell_ref(0, 26, true), "$AA$1");
        assert_eq!(cell_ref(MAX_ROWS - 1, 16_383, false), "XFD1048576");
    }
}
