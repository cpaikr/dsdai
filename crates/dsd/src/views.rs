//! Read-side views: `read`, `outline`, `table`, `find`, and `inspect`.
//!
//! Each view is a serializable structure for the JSON contract plus a compact
//! text rendering for `--text`.

use std::collections::BTreeMap;
use std::fmt::Write as _;

use serde::Serialize;

use crate::archive::Archive;
use crate::check::{Issue, check_archive, meta_generator, parse_meta};
use crate::doc::{Doc, is_scaffold, is_verified};
use crate::error::{Error, Result};
use crate::text::{
    INLINE_TAGS, LINE_BREAK_MARK, attr_label, blocks, collapse, compact, exact_text, header_info, node_text, prefix,
    quote, unverified_ancestor_tags, unverified_schema, unverified_tags,
};
use crate::xml::{Item, NodeId};

// ---------------------------------------------------------------------------
// read

const TEXT_VIEW_TAGS: [&str; 10] = [
    "P",
    "TITLE",
    "COVER-TITLE",
    "IMG",
    "IMG-CAPTION",
    "WARNING",
    "DOCUMENT-NAME",
    "FORMULA-VERSION",
    "COMPANY-NAME",
    "EXTRACTION",
];
/// A region starts a fresh path, so paths stay short: SECTION-2/P rather than
/// BODY/INSERTION/LIBRARY/SECTION-1/SECTION-2/P. `outline` shows the nesting.
const REGION_TAGS: [&str; 12] = [
    "DOCUMENT-HEADER",
    "COVER",
    "TOC",
    "PART",
    "APPENDIX",
    "SECTION-1",
    "SECTION-2",
    "SECTION-3",
    "SECTION-4",
    "SECTION-5",
    "SECTION-6",
    "SECTION-7",
];
const TRANSPARENT_TAGS: [&str; 2] = ["DOCUMENT", "BODY"];
const OTHER_CONTAINER_TAGS: [&str; 5] = ["SUMMARY", "INSERTION", "LIBRARY", "TABLE-GROUP", "IMAGE"];
const LAYOUT_SPACE: [char; 4] = [' ', '\t', '\r', '\n'];

fn shown_attrs(tag: &str) -> &'static [&'static str] {
    match tag {
        "DOCUMENT-NAME" | "EXTRACTION" => &["ACODE"],
        _ => &[],
    }
}

fn is_container(tag: &str) -> bool {
    REGION_TAGS.contains(&tag) || TRANSPARENT_TAGS.contains(&tag) || OTHER_CONTAINER_TAGS.contains(&tag)
}

#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct TableStub {
    pub index: usize,
    pub rows: usize,
    pub cols: usize,
    #[serde(skip_serializing_if = "Vec::is_empty")]
    pub unverified: Vec<String>,
}

#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct ViewRow {
    pub line: usize,
    pub path: String,
    /// Exact text; absent for a table stub or an element line without its own text.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub text: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub table: Option<TableStub>,
}

#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct ReadView {
    pub source: String,
    pub schema: String,
    pub editor_version: String,
    pub tables: usize,
    pub images: usize,
    pub unverified: Vec<String>,
    pub rows: Vec<ViewRow>,
}

struct TextView<'a> {
    doc: &'a Doc,
    scaffold: bool,
    mark: &'a str,
    rows: Vec<ViewRow>,
}

impl TextView<'_> {
    fn add(&mut self, offset: usize, path: &[String], text: Option<String>, table: Option<TableStub>) {
        self.rows.push(ViewRow { line: self.doc.line(offset), path: path.join("/"), text, table });
    }

    fn exact(&self, node: NodeId) -> String {
        exact_text(self.doc, node, self.scaffold, true, self.mark)
    }

    fn walk(&mut self, node: NodeId, path: &[String], keep_path: bool) {
        let tree = &self.doc.tree;
        let mut pos = tree.node(node).open_end;
        for item in &tree.node(node).items {
            match item {
                Item::Text(t) => {
                    let text = t.replace(['\r', '\n'], "");
                    if !text.trim_matches(LAYOUT_SPACE).is_empty() {
                        // The tree folds CRLF into LF, so measure the layout in the raw bytes.
                        let skip = self.doc.data[pos..]
                            .iter()
                            .take_while(|b| matches!(b, b' ' | b'\t' | b'\r' | b'\n'))
                            .count();
                        let mut p = path.to_vec();
                        p.push("#text".into());
                        let shown = text.replace(crate::doc::CR_TOKEN, self.mark);
                        self.add(pos + skip, &p, Some(shown), None);
                    }
                }
                Item::Node(child) => {
                    self.visit(*child, path, keep_path);
                    pos = tree.node(*child).end;
                }
            }
        }
    }

    fn visit(&mut self, node: NodeId, path: &[String], keep_path: bool) {
        let doc = self.doc;
        let tree = &doc.tree;
        let tag = tree.tag(node).to_owned();
        let start = tree.node(node).start;
        let with = |seg: String| {
            let mut p = path.to_vec();
            p.push(seg);
            p
        };
        if is_scaffold(&tag) {
            if self.scaffold {
                let text = exact_text(doc, node, true, true, self.mark);
                self.add(start, &with(tag), Some(text), None);
            }
            return;
        }
        let known = is_verified(&tag);
        if known && let Some(table) = doc.table_for_node(node) {
            let stub = TableStub {
                index: table.index,
                rows: table.rows.len(),
                cols: table.ncols(),
                unverified: unverified_tags(tree, node),
            };
            self.add(start, &with(tag), None, Some(stub));
        } else if known
            && tag == "PGBRK"
            && tree.children(node).is_empty()
            && exact_text(doc, node, self.scaffold, false, self.mark).trim_matches(LAYOUT_SPACE).is_empty()
        {
            self.add(start, &with(tag), None, None);
        } else if known && TEXT_VIEW_TAGS.contains(&tag.as_str()) {
            let seg = format!("{tag}{}", attr_label(tree, node, Some(shown_attrs(&tag))));
            let text = self.exact(node);
            self.add(start, &with(seg), Some(text), None);
        } else if known && is_container(&tag) {
            let inner = if TRANSPARENT_TAGS.contains(&tag.as_str()) {
                path.to_vec()
            } else if REGION_TAGS.contains(&tag.as_str()) && !keep_path {
                vec![tag]
            } else {
                with(tag)
            };
            self.walk(node, &inner, keep_path);
        } else {
            // Generic rule: unverified elements, and verified ones somewhere the
            // rules above do not expect them (a TR outside any TABLE, say).
            let seg = with(format!("{tag}{}", attr_label(tree, node, None)));
            let inline_only = tree
                .children(node)
                .iter()
                .filter(|&&c| self.scaffold || !is_scaffold(tree.tag(c)))
                .all(|&c| INLINE_TAGS.contains(&tree.tag(c)));
            if inline_only {
                let text = self.exact(node);
                self.add(start, &seg, Some(text), None);
            } else {
                self.add(start, &seg, None, None);
                self.walk(node, &seg, true);
            }
        }
    }
}

fn editor_version(arc: &Archive) -> String {
    arc.meta().ok().and_then(|m| parse_meta(m).ok()).and_then(|m| meta_generator(&m, "editver")).unwrap_or_default()
}

/// Every block in document order with its line, element path, and exact text.
///
/// `mark` replaces DART's `&cr;` line break: `\n` for JSON, `⏎` for text.
pub fn read_view(arc: &Archive, doc: &Doc, scaffold: bool, mark: &str) -> ReadView {
    let mut view = TextView { doc, scaffold, mark, rows: Vec::new() };
    view.visit(doc.root(), &[], false);
    let mut unverified = unverified_tags(&doc.tree, doc.root());
    if let Some(schema) = unverified_schema(doc) {
        unverified.insert(0, format!("schema {schema}"));
    }
    ReadView {
        source: arc.file_name(),
        schema: header_info(doc).schema,
        editor_version: editor_version(arc),
        tables: doc.tables.len(),
        images: arc.images().count(),
        unverified,
        rows: view.rows,
    }
}

fn py_json_list(items: &[String]) -> String {
    format!("[{}]", items.iter().map(|s| quote(s)).collect::<Vec<_>>().join(", "))
}

pub fn render_read(view: &ReadView, header: bool, tool: &str) -> String {
    let mut out = String::new();
    if header {
        let mut fields: Vec<(&str, String)> = vec![
            ("source", quote(&view.source)),
            ("schema", quote(&view.schema)),
            ("editor_version", quote(&view.editor_version)),
            ("tables", view.tables.to_string()),
            ("images", view.images.to_string()),
            ("converter", quote(tool)),
        ];
        if !view.unverified.is_empty() {
            fields.push(("unverified", py_json_list(&view.unverified)));
        }
        fields.push((
            "legend",
            quote(&format!(
                "L<line> <path>[attrs] \"exact text\"; {LINE_BREAK_MARK} = &cr; line break; ⟦TAG⟧…⟦/TAG⟧ = element inside text; T<n> -> run `dsd table FILE n` for cells"
            )),
        ));
        for (k, v) in fields {
            let _ = writeln!(out, "# {k}: {v}");
        }
        out.push('\n');
    }
    let width = view.rows.iter().map(|r| r.path.chars().count()).max().unwrap_or(0).min(40);
    for row in &view.rows {
        let rest = match (&row.text, &row.table) {
            (Some(text), _) => quote(text),
            (None, Some(t)) => {
                let mut s = format!("T{} rows={} cols={} -> table {}", t.index, t.rows, t.cols, t.index);
                if !t.unverified.is_empty() {
                    let _ = write!(s, " [contains unverified {}]", t.unverified.join(","));
                }
                s
            }
            (None, None) => String::new(),
        };
        let line = format!("L{:<6} {:<width$} {rest}", row.line, row.path);
        out.push_str(line.trim_end());
        out.push('\n');
    }
    out
}

// ---------------------------------------------------------------------------
// outline

#[derive(Debug, Clone, Serialize)]
#[serde(tag = "kind", rename_all = "kebab-case")]
pub enum OutlineItem {
    Section {
        tag: String,
        title: String,
        unverified: bool,
    },
    Unverified {
        tag: String,
    },
    CoverTitle {
        title: String,
    },
    Region {
        tag: String,
        label: String,
    },
    Insertion {
        filled: bool,
        note: String,
    },
    #[serde(rename_all = "camelCase")]
    Table {
        index: usize,
        rows: usize,
        cols: usize,
        aclass: String,
        first_text: String,
    },
    Image {
        name: String,
    },
    Text {
        tag: String,
        text: String,
    },
}

#[derive(Debug, Clone, Serialize)]
pub struct OutlineEntry {
    pub line: usize,
    pub depth: usize,
    #[serde(flatten)]
    pub item: OutlineItem,
}

pub fn outline(doc: &Doc, all: bool) -> Vec<OutlineEntry> {
    fn walk(doc: &Doc, node: NodeId, depth: usize, all: bool, out: &mut Vec<OutlineEntry>) {
        let tree = &doc.tree;
        for &child in tree.children(node) {
            let tag = tree.tag(child);
            let line = doc.line(tree.node(child).start);
            let mut push = |item| out.push(OutlineEntry { line, depth, item });
            if tag == "PART" || tag.starts_with("SECTION-") {
                let title =
                    tree.child_tag(child, "TITLE").map(|t| collapse(&node_text(tree, t, false))).unwrap_or_default();
                push(OutlineItem::Section { tag: tag.into(), title, unverified: !is_verified(tag) });
                walk(doc, child, depth + 1, all, out);
            } else if !is_verified(tag) {
                push(OutlineItem::Unverified { tag: tag.into() });
                walk(doc, child, depth + 1, all, out);
            } else if tag == "COVER-TITLE" {
                push(OutlineItem::CoverTitle { title: collapse(&node_text(tree, child, false)) });
            } else if ["COVER", "TOC", "BODY", "LIBRARY", "TABLE-GROUP", "APPENDIX"].contains(&tag) {
                let n = tree.node(child);
                let label = n.attr("ACLASS").filter(|s| !s.is_empty()).or_else(|| n.attr("ATYPE")).unwrap_or("");
                if ["COVER", "TOC", "APPENDIX"].contains(&tag) {
                    push(OutlineItem::Region { tag: tag.into(), label: label.into() });
                }
                walk(doc, child, depth + usize::from(tag == "COVER" || tag == "TOC"), all, out);
            } else if tag == "INSERTION" {
                let filled = tree.children(child).iter().any(|&c| tree.tag(c) == "LIBRARY");
                if filled || all {
                    let note = tree
                        .child_tag(child, "COMMENT")
                        .map(|c| collapse(&node_text(tree, c, true)))
                        .unwrap_or_default();
                    push(OutlineItem::Insertion { filled, note: prefix(&note, 60) });
                }
                walk(doc, child, depth, all, out);
            } else if let Some(t) = doc.table_for_node(child) {
                let first = t
                    .cells
                    .iter()
                    .map(|c| collapse(&node_text(tree, c.node, false)))
                    .find(|s| !s.is_empty())
                    .unwrap_or_default();
                push(OutlineItem::Table {
                    index: t.index,
                    rows: t.rows.len(),
                    cols: t.ncols(),
                    aclass: tree.node(child).attr("ACLASS").unwrap_or("").into(),
                    first_text: prefix(&first, 50),
                });
            } else if tag == "IMAGE" {
                let name = tree.first_tag(child, "IMG").map(|i| node_text(tree, i, false)).unwrap_or_default();
                push(OutlineItem::Image { name });
            } else if all
                && ["P", "WARNING", "TITLE"].contains(&tag)
                && !(tag == "TITLE" && (tree.tag(node) == "PART" || tree.tag(node).starts_with("SECTION-")))
            {
                let text = collapse(&node_text(tree, child, false));
                if !text.is_empty() {
                    push(OutlineItem::Text { tag: tag.into(), text: prefix(&text, 70) });
                }
            } else if !is_scaffold(tag) && !["P", "PGBRK", "TITLE", "WARNING"].contains(&tag) {
                walk(doc, child, depth, all, out);
            }
        }
    }
    let start = doc.tree.first_tag(doc.root(), "BODY").unwrap_or(doc.root());
    let mut out = Vec::new();
    walk(doc, start, 0, all, &mut out);
    out
}

pub fn render_outline(entries: &[OutlineEntry]) -> String {
    let mut out = String::new();
    for e in entries {
        let pad = "  ".repeat(e.depth);
        let body = match &e.item {
            OutlineItem::Section { tag, title, unverified } => {
                format!("{tag} {title}{}", if *unverified { " [unverified element]" } else { "" })
            }
            OutlineItem::Unverified { tag } => format!("<{tag}> unverified element (contents shown below as found)"),
            OutlineItem::CoverTitle { title } => format!("COVER-TITLE {title}"),
            OutlineItem::Region { tag, label } => format!("{tag} {label}").trim_end().to_owned(),
            OutlineItem::Insertion { filled, note } => {
                format!("INSERTION {} {note}", if *filled { "filled" } else { "empty" })
            }
            OutlineItem::Table { index, rows, cols, aclass, first_text } => {
                format!("T{index} {rows}x{cols} {aclass} {first_text}")
            }
            OutlineItem::Image { name } => format!("IMAGE {name}"),
            OutlineItem::Text { tag, text } => format!("{tag} {text}"),
        };
        let _ = writeln!(out, "{:>7}  {pad}{body}", e.line);
    }
    out
}

// ---------------------------------------------------------------------------
// table

#[derive(Debug, Clone, Serialize)]
#[serde(untagged)]
pub enum CellView {
    #[serde(rename_all = "camelCase")]
    Origin {
        col: usize,
        tag: String,
        text: String,
        #[serde(skip_serializing_if = "is_one")]
        rowspan: usize,
        #[serde(skip_serializing_if = "is_one")]
        colspan: usize,
        #[serde(skip_serializing_if = "BTreeMap::is_empty")]
        attrs: BTreeMap<String, String>,
        #[serde(skip_serializing_if = "std::ops::Not::not")]
        mixed_content: bool,
    },
    #[serde(rename_all = "camelCase")]
    Covered {
        col: usize,
        covered_by: String,
    },
    Missing {
        col: usize,
        missing: bool,
    },
}

#[allow(clippy::trivially_copy_pass_by_ref)]
fn is_one(v: &usize) -> bool {
    *v == 1
}

#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct RowView {
    pub row: usize,
    pub line: usize,
    pub section: String,
    pub acopy: String,
    pub adelete: String,
    pub cells: Vec<CellView>,
}

#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct TableView {
    pub index: usize,
    pub row_count: usize,
    pub col_count: usize,
    pub line: usize,
    pub aclass: String,
    pub border: String,
    pub afixtable: String,
    pub col_widths: Vec<i64>,
    #[serde(skip_serializing_if = "Vec::is_empty")]
    pub contains_unverified: Vec<String>,
    #[serde(skip_serializing_if = "Vec::is_empty")]
    pub inside_unverified: Vec<String>,
    pub rows: Vec<RowView>,
}

/// Whether a cell's text can be replaced through a single P/SPAN wrapper.
pub fn leaf_for_text(doc: &Doc, cell: NodeId) -> Option<NodeId> {
    let tree = &doc.tree;
    let mut node = cell;
    loop {
        let kids = tree.children(node);
        let loose: String = tree
            .node(node)
            .items
            .iter()
            .filter_map(|i| if let Item::Text(t) = i { Some(t.as_str()) } else { None })
            .collect();
        if kids.is_empty() {
            return Some(node);
        }
        if kids.len() == 1 && loose.trim().is_empty() && ["P", "SPAN"].contains(&tree.tag(kids[0])) {
            node = kids[0];
            continue;
        }
        return None;
    }
}

pub fn table_view(doc: &Doc, index: usize, spans: bool) -> Result<TableView> {
    let table = doc.table(index)?;
    let tree = &doc.tree;
    let n = tree.node(table.node);
    let mut rows = Vec::new();
    for (ri, &tr) in table.rows.iter().enumerate() {
        let trn = tree.node(tr);
        let mut cells = Vec::new();
        for ci in 0..table.ncols() {
            let col = ci + 1;
            let Some(cell) = table.cell_at_grid(ri, ci) else {
                cells.push(CellView::Missing { col, missing: true });
                continue;
            };
            if (cell.row, cell.col) != (ri + 1, col) {
                if cell.col == col || spans {
                    cells.push(CellView::Covered { col, covered_by: format!("R{}C{}", cell.row, cell.col) });
                }
                continue;
            }
            let cn = tree.node(cell.node);
            let attrs = ["ACODE", "AUNIT", "AUNITVALUE"]
                .iter()
                .filter_map(|k| cn.attr(k).map(|v| ((*k).to_owned(), v.to_owned())))
                .collect();
            cells.push(CellView::Origin {
                col,
                tag: cn.tag.clone(),
                text: node_text(tree, cell.node, false),
                rowspan: cell.rowspan,
                colspan: cell.colspan,
                attrs,
                mixed_content: leaf_for_text(doc, cell.node).is_none(),
            });
        }
        rows.push(RowView {
            row: ri + 1,
            line: doc.line(trn.start),
            section: trn.parent.map(|p| tree.tag(p).to_owned()).unwrap_or_default(),
            acopy: trn.attr("ACOPY").unwrap_or("").into(),
            adelete: trn.attr("ADELETE").unwrap_or("").into(),
            cells,
        });
    }
    Ok(TableView {
        index,
        row_count: table.rows.len(),
        col_count: table.ncols(),
        line: doc.line(n.start),
        aclass: n.attr("ACLASS").unwrap_or("").into(),
        border: n.attr("BORDER").unwrap_or("").into(),
        afixtable: n.attr("AFIXTABLE").unwrap_or("").into(),
        col_widths: table.cols.clone(),
        contains_unverified: unverified_tags(tree, table.node),
        inside_unverified: unverified_ancestor_tags(tree, table.node),
        rows,
    })
}

pub fn render_table(t: &TableView) -> String {
    let mut out = String::new();
    let widths = t.col_widths.iter().map(ToString::to_string).collect::<Vec<_>>().join(", ");
    let _ = writeln!(
        out,
        "T{} rows={} cols={} line={} ACLASS={} BORDER={} AFIXTABLE={} COL=[{widths}]",
        t.index, t.row_count, t.col_count, t.line, t.aclass, t.border, t.afixtable
    );
    for (relation, tags) in [("contains", &t.contains_unverified), ("lies inside", &t.inside_unverified)] {
        if !tags.is_empty() {
            let names = tags.iter().map(|t| format!("<{t}>")).collect::<Vec<_>>().join(", ");
            let _ = writeln!(
                out,
                "  warning: {relation} unverified {names}; coordinates may not match DART Editor and edit commands refuse this table"
            );
        }
    }
    for r in &t.rows {
        let _ = writeln!(out, "  R{} line={} {} ACOPY={} ADELETE={}", r.row, r.line, r.section, r.acopy, r.adelete);
        for c in &r.cells {
            match c {
                CellView::Missing { col, .. } => {
                    let _ = writeln!(out, "    C{col} <missing>");
                }
                CellView::Covered { col, covered_by } => {
                    let _ = writeln!(out, "    C{col} ^ covered by {covered_by}");
                }
                CellView::Origin { col, tag, text, rowspan, colspan, attrs, mixed_content } => {
                    let mut extra = Vec::new();
                    if *rowspan > 1 {
                        extra.push(format!("rowspan={rowspan}"));
                    }
                    if *colspan > 1 {
                        extra.push(format!("colspan={colspan}"));
                    }
                    for k in ["ACODE", "AUNIT", "AUNITVALUE"] {
                        if let Some(v) = attrs.get(k) {
                            extra.push(format!("{k}={v}"));
                        }
                    }
                    if *mixed_content {
                        extra.push("mixed-content".into());
                    }
                    let line =
                        format!("    C{col} {tag} {} {}", quote(&text.replace('\n', LINE_BREAK_MARK)), extra.join(" "));
                    out.push_str(line.trim_end());
                    out.push('\n');
                }
            }
        }
    }
    out
}

// ---------------------------------------------------------------------------
// find

#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct FindMatch {
    pub locator: String,
    pub line: usize,
    pub tag: String,
    pub text: String,
    #[serde(skip_serializing_if = "Vec::is_empty")]
    pub unverified: Vec<String>,
}

#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct FindResult {
    pub query: String,
    pub exact: bool,
    pub matches: Vec<FindMatch>,
    /// True when the search stopped because it reached the limit.
    pub limited: bool,
}

pub fn find(doc: &Doc, query: &str, exact: bool, limit: usize) -> Result<FindResult> {
    let target = if exact { query.to_owned() } else { compact(query) };
    if target.is_empty() {
        return Err(Error::invalid("the search text is empty"));
    }
    let mut matches = Vec::new();
    let mut limited = false;
    for block in blocks(doc) {
        let hay = if exact { block.text.clone() } else { compact(&block.text) };
        if hay.contains(&target) {
            matches.push(FindMatch {
                locator: block.locator,
                line: block.line,
                tag: block.tag,
                text: block.text,
                unverified: block.unverified,
            });
            if matches.len() == limit {
                limited = true;
                break;
            }
        }
    }
    Ok(FindResult { query: query.to_owned(), exact, matches, limited })
}

pub fn render_find(r: &FindResult) -> String {
    let mut out = String::new();
    for m in &r.matches {
        let flag =
            if m.unverified.is_empty() { String::new() } else { format!(" [unverified {}]", m.unverified.join(",")) };
        let _ = writeln!(
            out,
            "{:>14}  line={:<6} {:11} {}{flag}",
            m.locator,
            m.line,
            m.tag,
            prefix(&collapse(&m.text), 120)
        );
    }
    if r.limited {
        let _ = writeln!(out, "... stopped after {} matches (raise --limit)", r.matches.len());
    }
    if r.matches.is_empty() {
        out.push_str("no matches\n");
    }
    out
}

// ---------------------------------------------------------------------------
// inspect

#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct MemberInfo {
    pub name: String,
    pub name_encoding: &'static str,
    pub utf8_flag: bool,
    pub method: &'static str,
    pub size: u64,
}

#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct MetaInfo {
    pub schema: Option<String>,
    pub editor_version: Option<String>,
    pub doc_version: Option<String>,
    pub doc_id: Option<String>,
    pub is_correction: Option<String>,
}

#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct DocumentInfo {
    pub schema: String,
    #[serde(rename = "type")]
    pub kind: Option<String>,
    pub type_code: Option<String>,
    pub formula_version: Option<String>,
    pub line_endings: &'static str,
    pub counts: BTreeMap<String, usize>,
    pub span_tables: usize,
}

#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct Inspection {
    pub file: String,
    pub bytes: u64,
    pub members: Vec<MemberInfo>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub meta: Option<MetaInfo>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub document: Option<DocumentInfo>,
    pub issues: Vec<Issue>,
}

const COUNTED_TAGS: [&str; 19] = [
    "PART",
    "SECTION-1",
    "SECTION-2",
    "SECTION-3",
    "TITLE",
    "P",
    "TABLE",
    "TR",
    "TH",
    "TD",
    "TE",
    "TU",
    "INSERTION",
    "LIBRARY",
    "IMAGE",
    "PGBRK",
    "WARNING",
    "APPENDIX",
    "NOTE",
];

/// Archive, metadata, and structure counts; no document text.
pub fn inspect(arc: &Archive) -> Result<Inspection> {
    let (issues, doc) = check_archive(arc);
    let bytes = std::fs::metadata(&arc.path).map_err(|e| Error::io(&arc.path, &e))?.len();
    let members = arc
        .members
        .iter()
        .map(|m| MemberInfo {
            name: m.name.clone(),
            name_encoding: m.name_encoding.as_str(),
            utf8_flag: m.utf8_flag(),
            method: m.method_name(),
            size: m.size,
        })
        .collect();
    let meta = arc.meta().ok().and_then(|data| parse_meta(data).ok()).map(|meta| {
        let attr = |tag: &str, name: &str| {
            meta.child_tag(meta.root, tag).and_then(|n| meta.node(n).attr(name)).map(str::to_owned)
        };
        MetaInfo {
            schema: attr("GENERATOR", "schema"),
            editor_version: attr("GENERATOR", "editver"),
            doc_version: attr("DOCUMENT-INFO", "docver"),
            doc_id: attr("DOCUMENT-INFO", "doc-id"),
            is_correction: attr("DOCUMENT-INFO", "iscorrection"),
        }
    });
    let document = doc.map(|doc| {
        let head = header_info(&doc);
        let mut counts = BTreeMap::new();
        for n in doc.tree.descendants(doc.root()) {
            let tag = doc.tag(n);
            if COUNTED_TAGS.contains(&tag) {
                *counts.entry(tag.to_owned()).or_insert(0) += 1;
            }
        }
        DocumentInfo {
            schema: head.schema,
            kind: head.document_name,
            type_code: head.document_code,
            formula_version: head.formula_version,
            line_endings: if doc.data.windows(2).any(|w| w == b"\r\n") { "CRLF" } else { "LF" },
            counts,
            span_tables: doc.tables.iter().filter(|t| t.has_spans()).count(),
        }
    });
    Ok(Inspection { file: arc.file_name(), bytes, members, meta, document, issues })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn text_view_rules_cover_only_verified_elements() {
        let ruled = TEXT_VIEW_TAGS
            .iter()
            .chain(&REGION_TAGS)
            .chain(&TRANSPARENT_TAGS)
            .chain(&OTHER_CONTAINER_TAGS)
            .chain(&INLINE_TAGS)
            .chain(&["TABLE", "PGBRK"]);
        for tag in ruled {
            assert!(is_verified(tag), "{tag}");
        }
    }
}
