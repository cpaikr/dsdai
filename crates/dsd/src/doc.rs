//! The parsed `contents.xml` document and its table grids.

use std::collections::HashMap;

use crate::error::{Error, Result};
use crate::xml::{self, NodeId, Tree};

pub const CR_TOKEN: &str = "&cr;"; // DART Editor's in-text line break, stored as &amp;cr;
pub const CELL_TAGS: [&str; 4] = ["TH", "TD", "TE", "TU"];
pub const ROW_SECTIONS: [&str; 3] = ["THEAD", "TBODY", "TFOOT"];
/// Editor-only guidance; never document content.
pub const SCAFFOLD_TAGS: [&str; 3] = ["COMMENT", "LIBRARYLIST", "FILENAME"];
pub const BLOCK_TEXT_TAGS: [&str; 6] = ["P", "TITLE", "COVER-TITLE", "IMG-CAPTION", "WARNING", "SUBTITLE"];

/// Elements seen in real DART Editor output. Add one only after verifying
/// rendering and edits on a real document that uses it.
pub const OBSERVED_TAGS: [&str; 41] = [
    "A",
    "APPENDIX",
    "BODY",
    "COL",
    "COLGROUP",
    "COMMENT",
    "COMPANY-NAME",
    "COVER",
    "COVER-TITLE",
    "DOCUMENT",
    "DOCUMENT-HEADER",
    "DOCUMENT-NAME",
    "EXTRACTION",
    "FILENAME",
    "FORMULA-VERSION",
    "IMAGE",
    "IMG",
    "IMG-CAPTION",
    "INSERTION",
    "LIBRARY",
    "LIBRARYLIST",
    "P",
    "PART",
    "PGBRK",
    "SECTION-1",
    "SECTION-2",
    "SECTION-3",
    "SPAN",
    "SUMMARY",
    "TABLE",
    "TABLE-GROUP",
    "TBODY",
    "TD",
    "TE",
    "TH",
    "THEAD",
    "TITLE",
    "TOC",
    "TR",
    "TU",
    "WARNING",
];
/// Never seen, but both DART schemas give them exactly the content model and
/// attributes of an observed element the rules already handle: SECTION-4..6
/// match SECTION-2, SECTION-7 matches it without a nested section, and TFOOT
/// matches TBODY.
pub const SCHEMA_EQUIVALENT_TAGS: [&str; 5] = ["SECTION-4", "SECTION-5", "SECTION-6", "SECTION-7", "TFOOT"];
pub const OBSERVED_SCHEMAS: [&str; 2] = ["dart3.xsd", "dart4.xsd"];

pub fn is_scaffold(tag: &str) -> bool {
    SCAFFOLD_TAGS.contains(&tag)
}

pub fn is_verified(tag: &str) -> bool {
    OBSERVED_TAGS.contains(&tag) || SCHEMA_EQUIVALENT_TAGS.contains(&tag)
}

pub fn is_cell(tag: &str) -> bool {
    CELL_TAGS.contains(&tag)
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Cell {
    pub node: NodeId,
    /// 1-based origin row.
    pub row: usize,
    /// 1-based origin column.
    pub col: usize,
    pub rowspan: usize,
    pub colspan: usize,
}

#[derive(Debug, Clone)]
pub struct Table {
    /// 1-based document order.
    pub index: usize,
    pub node: NodeId,
    pub cols: Vec<i64>,
    pub rows: Vec<NodeId>,
    /// Indexes into `cells` for each covered grid position.
    pub grid: Vec<Vec<Option<usize>>>,
    pub cells: Vec<Cell>,
    pub issues: Vec<String>,
}

impl Table {
    pub fn ncols(&self) -> usize {
        self.cols.len()
    }

    pub fn cell_at_grid(&self, row: usize, col: usize) -> Option<&Cell> {
        self.grid.get(row)?.get(col)?.map(|i| &self.cells[i])
    }

    pub fn has_spans(&self) -> bool {
        self.cells.iter().any(|c| c.rowspan > 1 || c.colspan > 1)
    }
}

#[derive(Debug, Clone)]
pub struct Doc {
    pub data: Vec<u8>,
    pub tree: Tree,
    newlines: Vec<usize>,
    pub tables: Vec<Table>,
    table_by_node: HashMap<NodeId, usize>,
}

fn int_attr(tree: &Tree, node: NodeId, name: &str) -> usize {
    tree.node(node)
        .attr(name)
        .and_then(|v| v.trim().parse::<i64>().ok())
        .map_or(1, |v| usize::try_from(v.max(1)).unwrap_or(1))
}

fn parse_int(value: &str) -> Option<i64> {
    let t = value.trim();
    let digits = t.strip_prefix(['+', '-']).unwrap_or(t);
    if digits.is_empty() || !digits.bytes().all(|b| b.is_ascii_digit()) {
        return None;
    }
    t.parse::<i64>().ok()
}

/// Python-compatible integer parsing of attribute values (`int(value)`).
pub fn attr_int(value: &str) -> Option<i64> {
    parse_int(value)
}

impl Doc {
    pub fn parse(data: Vec<u8>) -> Result<Self> {
        let tree = xml::parse(&data)?;
        let newlines = data.iter().enumerate().filter(|&(_, &b)| b == b'\n').map(|(i, _)| i).collect();
        let table_nodes = tree.iter_tag(tree.root, "TABLE");
        let tables: Vec<Table> = table_nodes.iter().enumerate().map(|(i, &n)| build_table(&tree, i + 1, n)).collect();
        let table_by_node = tables.iter().enumerate().map(|(i, t)| (t.node, i)).collect();
        Ok(Self { data, tree, newlines, tables, table_by_node })
    }

    pub fn root(&self) -> NodeId {
        self.tree.root
    }

    /// 1-based line of a byte offset.
    pub fn line(&self, offset: usize) -> usize {
        self.newlines.partition_point(|&nl| nl < offset) + 1
    }

    pub fn table(&self, index: usize) -> Result<&Table> {
        if index == 0 || index > self.tables.len() {
            return Err(Error::invalid(format!(
                "table {index} does not exist (document has {} tables)",
                self.tables.len()
            )));
        }
        Ok(&self.tables[index - 1])
    }

    pub fn table_for_node(&self, node: NodeId) -> Option<&Table> {
        self.table_by_node.get(&node).map(|&i| &self.tables[i])
    }

    pub fn newline(&self) -> &'static [u8] {
        let head = &self.data[..self.data.len().min(4096)];
        if head.windows(2).any(|w| w == b"\r\n") { b"\r\n" } else { b"\n" }
    }

    pub fn tag(&self, node: NodeId) -> &str {
        self.tree.tag(node)
    }

    pub fn attr(&self, node: NodeId, name: &str) -> Option<&str> {
        self.tree.node(node).attr(name)
    }

    /// The cell whose grid area covers 1-based (row, col).
    pub fn cell_at(&self, table: &Table, row: usize, col: usize) -> Result<Cell> {
        let t = table.index;
        if row == 0 || row > table.rows.len() {
            return Err(Error::invalid(format!("T{t} has rows 1..{}; R{row} does not exist", table.rows.len())));
        }
        if col == 0 || col > table.ncols() {
            return Err(Error::invalid(format!("T{t} has columns 1..{}; C{col} does not exist", table.ncols())));
        }
        table
            .cell_at_grid(row - 1, col - 1)
            .copied()
            .ok_or_else(|| Error::invalid(format!("T{t} R{row}C{col} is not covered by any cell")))
    }
}

pub fn build_table(tree: &Tree, index: usize, node: NodeId) -> Table {
    let mut cols: Vec<i64> = Vec::new();
    let mut rows: Vec<NodeId> = Vec::new();
    for &child in tree.children(node) {
        let tag = tree.tag(child);
        if tag == "COLGROUP" {
            for &col in tree.children(child) {
                if tree.tag(col) == "COL" {
                    cols.push(tree.node(col).attr("WIDTH").map_or(Some(0), parse_int).unwrap_or(0));
                }
            }
        } else if ROW_SECTIONS.contains(&tag) {
            rows.extend(tree.children(child).iter().copied().filter(|&r| tree.tag(r) == "TR"));
        } else if tag == "TR" {
            rows.push(child);
        }
    }
    let mut issues = Vec::new();
    if cols.is_empty() && !rows.is_empty() {
        // No COLGROUP: infer the grid width from the widest row (sizes unknown).
        let widest = rows
            .iter()
            .map(|&r| {
                tree.children(r)
                    .iter()
                    .filter(|&&c| is_cell(tree.tag(c)))
                    .map(|&c| int_attr(tree, c, "COLSPAN"))
                    .sum::<usize>()
            })
            .max()
            .unwrap_or(0);
        cols = vec![0; widest];
    }
    let ncols = cols.len();
    let nrows = rows.len();
    let mut grid: Vec<Vec<Option<usize>>> = vec![vec![None; ncols.max(1)]; nrows];
    let mut cells: Vec<Cell> = Vec::new();
    for (ri, &row) in rows.iter().enumerate() {
        let mut ci = 0;
        for &cnode in tree.children(row) {
            if !is_cell(tree.tag(cnode)) {
                continue;
            }
            while ci < ncols && grid[ri][ci].is_some() {
                ci += 1;
            }
            let (rs, cs) = (int_attr(tree, cnode, "ROWSPAN"), int_attr(tree, cnode, "COLSPAN"));
            let cell_index = cells.len();
            cells.push(Cell { node: cnode, row: ri + 1, col: ci + 1, rowspan: rs, colspan: cs });
            if ci + cs > ncols {
                issues.push(format!("R{}C{}: cell spans past the {ncols} declared columns", ri + 1, ci + 1));
            }
            if ri + rs > nrows {
                issues.push(format!("R{}C{}: ROWSPAN {rs} runs past the last row", ri + 1, ci + 1));
            }
            for a in 0..rs {
                for b in 0..cs {
                    if ri + a < nrows && ci + b < ncols {
                        if grid[ri + a][ci + b].is_some() {
                            issues.push(format!("R{}C{}: overlapping cells", ri + a + 1, ci + b + 1));
                        }
                        grid[ri + a][ci + b] = Some(cell_index);
                    }
                }
            }
            ci += cs;
        }
    }
    for (ri, row) in grid.iter().enumerate() {
        let missing: Vec<String> = (0..ncols).filter(|&ci| row[ci].is_none()).map(|ci| (ci + 1).to_string()).collect();
        if !missing.is_empty() {
            issues.push(format!("R{}: no cell covers column(s) {}", ri + 1, missing.join(",")));
        }
    }
    Table { index, node, cols, rows, grid, cells, issues }
}
