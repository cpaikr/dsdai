//! Structural edits: table rows and columns, whole tables, and paragraphs.
//!
//! Like every edit, these splice bytes into the original `contents.xml`.
//! Spans and sizes follow the size arithmetic DART Editor writes: a cell's
//! `HEIGHT` is the spanned rows' `TR HEIGHT` minus 7, and its `WIDTH` follows
//! the table's [`WidthConvention`]. A copied row or column joins every span
//! it lies inside, so a group such as "subsidiaries" grows by one entry; a
//! deleted one shrinks them, moving a spanning cell down when its own row goes.

use crate::check::{WidthConvention, width_convention};
use crate::doc::{BLOCK_TEXT_TAGS, Cell, Doc, Table, attr_int, is_cell};
use crate::edit::{Change, Splice, is_blank, replace_ranges, set_attr_in_tag, text_runs, xml_escape_text};
use crate::error::{Error, Result};
use crate::text::{node_text, prefix, quote, require_verified};
use crate::views::leaf_for_text;
use crate::xml::NodeId;

fn int_of(doc: &Doc, node: NodeId, name: &str) -> Option<i64> {
    doc.attr(node, name).and_then(attr_int)
}

fn locked(doc: &Doc, node: NodeId, attr: &str) -> bool {
    doc.attr(node, attr).is_some_and(|v| v.eq_ignore_ascii_case("N"))
}

/// Remove ` NAME="value"` from a start tag; unchanged when absent.
fn remove_attr_in_tag(tag: &[u8], name: &str) -> Vec<u8> {
    let name = name.as_bytes();
    let mut i = 0;
    while i < tag.len() {
        if crate::edit::is_ascii_space(tag[i]) && tag[i + 1..].starts_with(name) {
            let mut j = i + 1 + name.len();
            while j < tag.len() && crate::edit::is_ascii_space(tag[j]) {
                j += 1;
            }
            if tag.get(j) == Some(&b'=') {
                j += 1;
                while j < tag.len() && crate::edit::is_ascii_space(tag[j]) {
                    j += 1;
                }
                if let Some(&q) = tag.get(j).filter(|&&q| q == b'"' || q == b'\'')
                    && let Some(close) = tag[j + 1..].iter().position(|&b| b == q)
                {
                    return [&tag[..i], &tag[j + 2 + close..]].concat();
                }
            }
        }
        i += 1;
    }
    tag.to_vec()
}

/// A span attribute: written when above 1, dropped at 1 as DART Editor does.
fn with_span(tag: &[u8], name: &str, value: usize) -> Vec<u8> {
    if value > 1 { set_attr_in_tag(tag, name, &value.to_string()) } else { remove_attr_in_tag(tag, name) }
}

/// Add `delta` to a numeric size attribute; leave it alone when absent or not a number.
fn with_size(tag: &[u8], doc: &Doc, node: NodeId, name: &str, delta: i64) -> Vec<u8> {
    match int_of(doc, node, name) {
        Some(v) if delta != 0 => set_attr_in_tag(tag, name, &(v + delta).to_string()),
        _ => tag.to_vec(),
    }
}

fn start_tag<'a>(data: &'a [u8], doc: &Doc, node: NodeId) -> &'a [u8] {
    let n = doc.tree.node(node);
    &data[n.start..n.open_end]
}

/// The whitespace between `node` and the preceding sibling element (or its
/// parent's start tag), or the document newline when that gap holds text.
fn gap_before(doc: &Doc, node: NodeId) -> Vec<u8> {
    let tree = &doc.tree;
    let Some(parent) = tree.node(node).parent else { return doc.newline().to_vec() };
    let siblings = tree.children(parent);
    let i = siblings.iter().position(|&s| s == node).expect("a node is its parent's child");
    let prev_end = if i > 0 { tree.node(siblings[i - 1]).end } else { tree.node(parent).open_end };
    let gap = &doc.data[prev_end..tree.node(node).start];
    if is_blank(gap) && !gap.is_empty() { gap.to_vec() } else { doc.newline().to_vec() }
}

/// The byte range that removes `node` together with the gap before it.
fn removal(doc: &Doc, node: NodeId) -> (usize, usize) {
    let n = doc.tree.node(node);
    let gap = gap_before(doc, node);
    let start = if n.start >= gap.len() && doc.data[n.start - gap.len()..n.start] == gap[..] {
        n.start - gap.len()
    } else {
        n.start
    };
    (start, n.end)
}

/// Empty every cell's text in a fragment (a row, cell, or table), keeping
/// wrappers and attributes; TU unit values are emptied too.
fn clear_cells(fragment: &[u8]) -> Result<(Vec<u8>, Vec<String>)> {
    let sub = Doc::parse(fragment.to_vec())?;
    let mut clears: Vec<Splice> = Vec::new();
    let mut kept = Vec::new();
    let cells: Vec<NodeId> = sub.tree.descendants(sub.root()).into_iter().filter(|&n| is_cell(sub.tag(n))).collect();
    for (k, &cnode) in cells.iter().enumerate() {
        let cn = sub.tree.node(cnode);
        let has_unit = cn.tag == "TU" && cn.attr("AUNITVALUE").is_some_and(|v| !v.is_empty());
        match leaf_for_text(&sub, cnode) {
            None => {
                // Nested cells clear themselves; take only this cell's own runs
                // so no range is edited twice.
                let nested: Vec<(usize, usize)> = sub.tree.descendants(cnode)[1..]
                    .iter()
                    .filter(|&&n| is_cell(sub.tag(n)))
                    .map(|&n| (sub.tree.node(n).start, sub.tree.node(n).end))
                    .collect();
                let mut runs = Vec::new();
                text_runs(&sub, cnode, &mut runs);
                clears.extend(
                    runs.into_iter()
                        .filter(|&(a, b)| !is_blank(&fragment[a..b]))
                        .filter(|&(a, _)| !nested.iter().any(|&(s, e)| s <= a && a < e))
                        .map(|(a, b)| (a, b, Vec::new())),
                );
                kept.push(format!("cell {} (structure kept, text emptied)", k + 1));
            }
            Some(leaf) => {
                let l = sub.tree.node(leaf);
                if l.close_start != l.open_end {
                    clears.push((l.open_end, l.close_start, Vec::new()));
                }
            }
        }
        if has_unit {
            clears.push((cn.start, cn.open_end, set_attr_in_tag(&fragment[cn.start..cn.open_end], "AUNITVALUE", "")));
        }
    }
    Ok((replace_ranges(fragment, clears)?, kept))
}

/// Most copies one command makes.
const MAX_COPIES: usize = 1000;

/// Refuse a copy count outside `1..=MAX_COPIES` or one that could grow
/// contents.xml past the archive's member limit (the whole table bounds what
/// one row, column, or table copy adds).
fn check_copies(doc: &Doc, table: &Table, count: usize) -> Result<()> {
    if !(1..=MAX_COPIES).contains(&count) {
        return Err(Error::invalid(format!("--count must be between 1 and {MAX_COPIES}")));
    }
    let n = doc.tree.node(table.node);
    let grown = (n.end - n.start).saturating_mul(count).saturating_add(doc.data.len());
    if u64::try_from(grown).unwrap_or(u64::MAX) > crate::archive::MAX_MEMBER_BYTES {
        return Err(Error::invalid(format!("{count} copies would make contents.xml too large")));
    }
    Ok(())
}

fn cleared_note(kept: &[String]) -> String {
    if kept.is_empty() { " (cleared)".into() } else { format!(" (cleared; {})", kept.join(", ")) }
}

fn verified_table(doc: &Doc, table_no: usize) -> Result<&Table> {
    let table = doc.table(table_no)?;
    require_verified(doc, table.node, &format!("T{table_no}"))?;
    if table.oversized {
        return Err(Error::refused(format!("T{table_no} is too large to map")));
    }
    if !table.issues.is_empty() {
        return Err(Error::refused(format!("T{table_no} has grid problems: {}", table.issues[0]))
            .with_hint("repair the table in DART Editor or by hand before structural edits"));
    }
    Ok(table)
}

fn check_row(table: &Table, row: usize) -> Result<()> {
    if row == 0 || row > table.rows.len() {
        return Err(Error::invalid(format!("T{} has rows 1..{}", table.index, table.rows.len())));
    }
    Ok(())
}

fn check_col(table: &Table, col: usize) -> Result<()> {
    if col == 0 || col > table.ncols() {
        return Err(Error::invalid(format!("T{} has columns 1..{}", table.index, table.ncols())));
    }
    Ok(())
}

fn lock_refusal(place: &str, attr: &str, what: &str) -> Error {
    Error::refused(format!("{place} is marked {attr}=\"N\" ({what} locked by the form)"))
        .with_hint("pass --force-locked only when the user wants to depart from the form")
}

fn span_label(c: &Cell) -> String {
    format!("R{}C{}", c.row, c.col)
}

// ---------------------------------------------------------------------------
// rows

/// Duplicate a row below itself. Copies join every ROWSPAN the row lies
/// inside: the spanning cell grows (ROWSPAN and HEIGHT) instead of being
/// duplicated.
pub fn copy_row(
    data: &[u8],
    table_no: usize,
    row: usize,
    count: usize,
    clear: bool,
    force: bool,
) -> Result<(Vec<u8>, Change)> {
    let doc = Doc::parse(data.to_vec())?;
    let table = verified_table(&doc, table_no)?;
    check_copies(&doc, table, count)?;
    check_row(table, row)?;
    let tr = table.rows[row - 1];
    if locked(&doc, tr, "ACOPY") && !force {
        return Err(lock_refusal(&format!("T{table_no}R{row}"), "ACOPY", "row copying"));
    }
    let tree = &doc.tree;
    let trn = tree.node(tr);
    let row_h = int_of(&doc, tr, "HEIGHT");
    let spans: Vec<&Cell> =
        table.cells.iter().filter(|c| c.rowspan > 1 && c.row <= row && row < c.row + c.rowspan).collect();
    // Cells of this row that span downward stay single: the copy sits inside them.
    let cuts: Vec<Splice> = spans
        .iter()
        .filter(|c| c.row == row)
        .map(|c| {
            let (a, b) = removal(&doc, c.node);
            (a - trn.start, b - trn.start, Vec::new())
        })
        .collect();
    if cuts.len() == table.cells.iter().filter(|c| c.row == row).count() {
        return Err(Error::refused(format!(
            "every cell of T{table_no}R{row} spans further rows; its copy would have no cells"
        ))
        .with_hint(format!("copy a row inside the spans instead, such as R{}", row + 1)));
    }
    let mut row_bytes = replace_ranges(&data[trn.start..trn.end], cuts)?;
    let mut note = String::new();
    if clear {
        let (cleared, kept) = clear_cells(&row_bytes)?;
        row_bytes = cleared;
        note = cleared_note(&kept);
    }
    let sep = gap_before(&doc, tr);
    let mut insertion = Vec::new();
    for _ in 0..count {
        insertion.extend_from_slice(&sep);
        insertion.extend_from_slice(&row_bytes);
    }
    let mut edits: Vec<Splice> = vec![(trn.end, trn.end, insertion)];
    let added = i64::try_from(count).unwrap_or(i64::MAX);
    let mut grown = Vec::new();
    for c in &spans {
        let tag = with_span(start_tag(data, &doc, c.node), "ROWSPAN", c.rowspan + count);
        let tag = with_size(&tag, &doc, c.node, "HEIGHT", row_h.map_or(0, |h| h * added));
        let n = tree.node(c.node);
        edits.push((n.start, n.open_end, tag));
        grown.push(format!("{} ROWSPAN {} -> {}", span_label(c), c.rowspan, c.rowspan + count));
    }
    let out = replace_ranges(data, edits)?;
    let mut change = Change::new(
        "copy-row",
        format!("T{table_no}: copied R{row} {count} time(s) as R{}..R{}{note}", row + 1, row + count),
    );
    if !grown.is_empty() {
        change.notes.push(format!("T{table_no}: the copies joined spanning cells ({})", grown.join(", ")));
    }
    Ok((out, change))
}

/// Delete a row. Spans that cross it shrink; a cell that spans down from this
/// row moves to the next row with one row less.
pub fn delete_row(data: &[u8], table_no: usize, row: usize, force: bool) -> Result<(Vec<u8>, Change)> {
    let doc = Doc::parse(data.to_vec())?;
    let table = verified_table(&doc, table_no)?;
    check_row(table, row)?;
    let tr = table.rows[row - 1];
    if locked(&doc, tr, "ADELETE") && !force {
        return Err(lock_refusal(&format!("T{table_no}R{row}"), "ADELETE", "row deletion"));
    }
    let tree = &doc.tree;
    let parent = tree.node(tr).parent.expect("a table row has a parent");
    if tree.children(parent).iter().filter(|&&c| tree.tag(c) == "TR").count() == 1 {
        return Err(Error::refused(format!(
            "T{table_no}R{row} is the only row in its {}; refusing to leave it empty",
            tree.tag(parent)
        )));
    }
    let row_h = int_of(&doc, tr, "HEIGHT").unwrap_or(0);
    let mut edits: Vec<Splice> = Vec::new();
    let mut notes = Vec::new();
    // Spans from above lose this row.
    for c in table.cells.iter().filter(|c| c.rowspan > 1 && c.row < row && row < c.row + c.rowspan) {
        let tag = with_span(start_tag(data, &doc, c.node), "ROWSPAN", c.rowspan - 1);
        let tag = with_size(&tag, &doc, c.node, "HEIGHT", -row_h);
        let n = tree.node(c.node);
        edits.push((n.start, n.open_end, tag));
        notes.push(format!("{} ROWSPAN {} -> {}", span_label(c), c.rowspan, c.rowspan - 1));
    }
    // Spans that start here move to the next row.
    let mut moving: Vec<&Cell> = table.cells.iter().filter(|c| c.rowspan > 1 && c.row == row).collect();
    moving.sort_by_key(|c| c.col);
    if !moving.is_empty() {
        let next = table.rows[row];
        if tree.node(next).parent != Some(parent) {
            return Err(Error::refused(format!(
                "T{table_no}R{row} anchors a ROWSPAN that continues into another row section"
            ))
            .with_hint("edit the XML by hand"));
        }
        let mut next_cells: Vec<&Cell> = table.cells.iter().filter(|c| c.row == row + 1).collect();
        next_cells.sort_by_key(|c| c.col);
        let mut inserts: std::collections::BTreeMap<usize, Vec<u8>> = std::collections::BTreeMap::new();
        for c in &moving {
            let n = tree.node(c.node);
            let tag = with_span(&data[n.start..n.open_end], "ROWSPAN", c.rowspan - 1);
            let tag = with_size(&tag, &doc, c.node, "HEIGHT", -row_h);
            let moved = [&tag[..], &data[n.open_end..n.end]].concat();
            if let Some(x) = next_cells.iter().find(|x| x.col > c.col) {
                let at = tree.node(x.node).start;
                let entry = inserts.entry(at).or_default();
                entry.extend_from_slice(&moved);
                entry.extend_from_slice(&gap_before(&doc, x.node));
            } else {
                let last = tree.children(next).iter().rev().find(|&&k| is_cell(tree.tag(k))).copied();
                let (at, gap) = match last {
                    Some(k) => (tree.node(k).end, gap_before(&doc, k)),
                    None => (tree.node(next).open_end, doc.newline().to_vec()),
                };
                let entry = inserts.entry(at).or_default();
                entry.extend_from_slice(&gap);
                entry.extend_from_slice(&moved);
            }
            notes.push(format!("{} moved to R{} with ROWSPAN {}", span_label(c), row + 1, c.rowspan - 1));
        }
        edits.extend(inserts.into_iter().map(|(at, bytes)| (at, at, bytes)));
    }
    let (a, b) = removal(&doc, tr);
    edits.push((a, b, Vec::new()));
    let text = table
        .cells
        .iter()
        .filter(|c| c.row == row)
        .map(|c| node_text(tree, c.node, false))
        .collect::<Vec<_>>()
        .join(" | ");
    let out = replace_ranges(data, edits)?;
    let mut change = Change::new("delete-row", format!("T{table_no}: deleted R{row} ({})", quote(&prefix(&text, 80))));
    if !notes.is_empty() {
        change.notes.push(format!("T{table_no}: spans adjusted ({})", notes.join(", ")));
    }
    Ok((out, change))
}

// ---------------------------------------------------------------------------
// columns

fn col_nodes(doc: &Doc, table: &Table) -> Option<(NodeId, Vec<NodeId>)> {
    let tree = &doc.tree;
    let group = tree.child_tag(table.node, "COLGROUP")?;
    let cols: Vec<NodeId> = tree.children(group).iter().copied().filter(|&c| tree.tag(c) == "COL").collect();
    (cols.len() == table.ncols()).then_some((group, cols))
}

/// New value of a table-level WIDTH (TABLE or COLGROUP) after `k` columns of
/// width `w` are added (negative `k` removes): the attribute keeps its relation
/// to the COL sum (equal, or plus 9 per column).
fn total_width(value: i64, col_sum: i64, ncols: i64, w: i64, k: i64) -> i64 {
    if value == col_sum + 9 * ncols { value + k * (w + 9) } else { value + k * w }
}

fn table_width_edits(data: &[u8], doc: &Doc, table: &Table, group: NodeId, w: i64, k: i64) -> Vec<Splice> {
    let col_sum: i64 = table.cols.iter().sum();
    let ncols = i64::try_from(table.ncols()).unwrap_or(i64::MAX);
    let mut out = Vec::new();
    for node in [table.node, group] {
        if let Some(v) = int_of(doc, node, "WIDTH") {
            let n = doc.tree.node(node);
            let value = total_width(v, col_sum, ncols, w, k).to_string();
            out.push((n.start, n.open_end, set_attr_in_tag(&data[n.start..n.open_end], "WIDTH", &value)));
        }
    }
    out
}

/// Duplicate a grid column to the right of itself. Cells spanning across it
/// widen (COLSPAN and WIDTH); the table and COLGROUP widths grow.
pub fn copy_col(
    data: &[u8],
    table_no: usize,
    col: usize,
    count: usize,
    clear: bool,
    force: bool,
) -> Result<(Vec<u8>, Change)> {
    let doc = Doc::parse(data.to_vec())?;
    let table = verified_table(&doc, table_no)?;
    check_copies(&doc, table, count)?;
    check_col(table, col)?;
    let Some((group, cols)) = col_nodes(&doc, table) else {
        return Err(
            Error::refused(format!("T{table_no} has no COLGROUP matching its grid")).with_hint("edit the XML by hand")
        );
    };
    let covering: Vec<&Cell> = table.cells.iter().filter(|c| c.col <= col && col < c.col + c.colspan).collect();
    if !force && let Some(c) = covering.iter().find(|c| c.colspan == 1 && locked(&doc, c.node, "ACOPYCOL")) {
        return Err(lock_refusal(&format!("T{table_no}{}", span_label(c)), "ACOPYCOL", "column copying"));
    }
    let tree = &doc.tree;
    let conv = width_convention(&doc, table);
    let w = table.cols[col - 1];
    let k = i64::try_from(count).unwrap_or(i64::MAX);
    let mut edits: Vec<Splice> = Vec::new();
    let mut kept_all = Vec::new();
    let mut widened = Vec::new();
    for c in &covering {
        let n = tree.node(c.node);
        if c.colspan == 1 {
            let mut bytes = data[n.start..n.end].to_vec();
            if clear {
                let (cleared, kept) = clear_cells(&bytes)?;
                bytes = cleared;
                kept_all.extend(kept.into_iter().map(|s| format!("{} {s}", span_label(c))));
            }
            let gap = gap_before(&doc, c.node);
            let mut insertion = Vec::new();
            for _ in 0..count {
                insertion.extend_from_slice(&gap);
                insertion.extend_from_slice(&bytes);
            }
            edits.push((n.end, n.end, insertion));
        } else {
            let tag = with_span(start_tag(data, &doc, c.node), "COLSPAN", c.colspan + count);
            let tag = with_size(&tag, &doc, c.node, "WIDTH", conv.per_column(w) * k);
            edits.push((n.start, n.open_end, tag));
            widened.push(format!("{} COLSPAN {} -> {}", span_label(c), c.colspan, c.colspan + count));
        }
    }
    let coln = tree.node(cols[col - 1]);
    let col_bytes = &data[coln.start..coln.end];
    let gap = gap_before(&doc, cols[col - 1]);
    let mut insertion = Vec::new();
    for _ in 0..count {
        insertion.extend_from_slice(&gap);
        insertion.extend_from_slice(col_bytes);
    }
    edits.push((coln.end, coln.end, insertion));
    edits.extend(table_width_edits(data, &doc, table, group, w, k));
    let out = replace_ranges(data, edits)?;
    let note = if clear { cleared_note(&kept_all) } else { String::new() };
    let mut change = Change::new(
        "copy-col",
        format!("T{table_no}: copied C{col} {count} time(s) as C{}..C{}{note}", col + 1, col + count),
    );
    if !widened.is_empty() {
        change.notes.push(format!("T{table_no}: spanning cells widened ({})", widened.join(", ")));
    }
    if conv == WidthConvention::Legacy {
        change.notes.push(format!("T{table_no}: sizes follow the table's Dart3-era WIDTH convention"));
    }
    Ok((out, change))
}

/// Delete a grid column. Cells spanning across it narrow; the table and
/// COLGROUP widths shrink.
pub fn delete_col(data: &[u8], table_no: usize, col: usize, force: bool) -> Result<(Vec<u8>, Change)> {
    let doc = Doc::parse(data.to_vec())?;
    let table = verified_table(&doc, table_no)?;
    check_col(table, col)?;
    if table.ncols() == 1 {
        return Err(Error::refused(format!("T{table_no} has one column; delete the table instead")));
    }
    let Some((group, cols)) = col_nodes(&doc, table) else {
        return Err(
            Error::refused(format!("T{table_no} has no COLGROUP matching its grid")).with_hint("edit the XML by hand")
        );
    };
    let covering: Vec<&Cell> = table.cells.iter().filter(|c| c.col <= col && col < c.col + c.colspan).collect();
    if !force && let Some(c) = covering.iter().find(|c| c.colspan == 1 && locked(&doc, c.node, "ADELETECOL")) {
        return Err(lock_refusal(&format!("T{table_no}{}", span_label(c)), "ADELETECOL", "column deletion"));
    }
    let tree = &doc.tree;
    for (ri, &tr) in table.rows.iter().enumerate() {
        let own: Vec<NodeId> = tree.children(tr).iter().copied().filter(|&k| is_cell(tree.tag(k))).collect();
        if !own.is_empty() && own.iter().all(|&k| covering.iter().any(|c| c.node == k && c.colspan == 1)) {
            return Err(Error::refused(format!("deleting T{table_no} C{col} would leave R{} without cells", ri + 1))
                .with_hint("delete that row first, or edit the XML by hand"));
        }
    }
    let conv = width_convention(&doc, table);
    let w = table.cols[col - 1];
    let mut edits: Vec<Splice> = Vec::new();
    let mut narrowed = Vec::new();
    let mut removed = Vec::new();
    for c in &covering {
        let n = tree.node(c.node);
        if c.colspan == 1 {
            let (a, b) = removal(&doc, c.node);
            edits.push((a, b, Vec::new()));
            let text = node_text(tree, c.node, false);
            if !text.is_empty() {
                removed.push(text);
            }
        } else {
            let tag = with_span(start_tag(data, &doc, c.node), "COLSPAN", c.colspan - 1);
            let tag = with_size(&tag, &doc, c.node, "WIDTH", -conv.per_column(w));
            edits.push((n.start, n.open_end, tag));
            narrowed.push(format!("{} COLSPAN {} -> {}", span_label(c), c.colspan, c.colspan - 1));
        }
    }
    let (a, b) = removal(&doc, cols[col - 1]);
    edits.push((a, b, Vec::new()));
    edits.extend(table_width_edits(data, &doc, table, group, w, -1));
    let out = replace_ranges(data, edits)?;
    let shown = prefix(&removed.join(" | "), 80);
    let mut change = Change::new("delete-col", format!("T{table_no}: deleted C{col} ({})", quote(&shown)));
    if !narrowed.is_empty() {
        change.notes.push(format!("T{table_no}: spanning cells narrowed ({})", narrowed.join(", ")));
    }
    Ok((out, change))
}

// ---------------------------------------------------------------------------
// whole tables

/// The TABLE-GROUP a table belongs to, if any: form-managed cover and period tables.
fn table_group(doc: &Doc, table: &Table) -> Option<NodeId> {
    doc.tree.node(table.node).parent.filter(|&p| doc.tag(p) == "TABLE-GROUP")
}

/// Duplicate a table right after itself (copies become T{n+1}..).
pub fn copy_table(data: &[u8], table_no: usize, count: usize, clear: bool, force: bool) -> Result<(Vec<u8>, Change)> {
    let doc = Doc::parse(data.to_vec())?;
    let table = verified_table(&doc, table_no)?;
    check_copies(&doc, table, count)?;
    if !force && table_group(&doc, table).is_some() {
        return Err(Error::refused(format!(
            "T{table_no} belongs to a TABLE-GROUP the form manages (cover or period block)"
        ))
        .with_hint("pass --force-locked only when the user wants to depart from the form"));
    }
    let n = doc.tree.node(table.node);
    let mut bytes = data[n.start..n.end].to_vec();
    let mut note = String::new();
    if clear {
        let (cleared, kept) = clear_cells(&bytes)?;
        bytes = cleared;
        note = cleared_note(&kept);
    }
    let gap = gap_before(&doc, table.node);
    let mut insertion = Vec::new();
    for _ in 0..count {
        insertion.extend_from_slice(&gap);
        insertion.extend_from_slice(&bytes);
    }
    let nested = doc
        .tables
        .iter()
        .filter(|t| t.index > table_no && n.start < doc.tree.node(t.node).start)
        .filter(|t| doc.tree.node(t.node).end <= n.end)
        .count();
    let first = table_no + nested + 1;
    let last = table_no + (nested + 1) * count;
    let out = replace_ranges(data, vec![(n.end, n.end, insertion)])?;
    let mut change =
        Change::new("copy-table", format!("T{table_no}: copied {count} time(s) as T{first}..T{last}{note}"));
    change.notes.push(format!("tables after T{table_no} are renumbered by +{}", (nested + 1) * count));
    Ok((out, change))
}

/// Remove a table.
pub fn delete_table(data: &[u8], table_no: usize, force: bool) -> Result<(Vec<u8>, Change)> {
    let doc = Doc::parse(data.to_vec())?;
    let table = verified_table(&doc, table_no)?;
    if !force && table_group(&doc, table).is_some() {
        return Err(Error::refused(format!(
            "T{table_no} belongs to a TABLE-GROUP the form manages (cover or period block)"
        ))
        .with_hint("pass --force-locked only when the user wants to depart from the form"));
    }
    if doc.tree.ancestors(table.node).iter().any(|&a| is_cell(doc.tag(a))) {
        return Err(
            Error::refused(format!("T{table_no} is nested inside a table cell")).with_hint("edit the XML by hand")
        );
    }
    let first = table
        .cells
        .iter()
        .map(|c| crate::text::collapse(&node_text(&doc.tree, c.node, false)))
        .find(|s| !s.is_empty())
        .unwrap_or_default();
    let tn = doc.tree.node(table.node);
    let nested = doc
        .tables
        .iter()
        .filter(|t| t.index > table_no && tn.start < doc.tree.node(t.node).start)
        .filter(|t| doc.tree.node(t.node).end <= tn.end)
        .count();
    let (a, b) = removal(&doc, table.node);
    let out = replace_ranges(data, vec![(a, b, Vec::new())])?;
    let mut change = Change::new(
        "delete-table",
        format!(
            "deleted T{table_no} ({} rows x {} columns, {})",
            table.rows.len(),
            table.ncols(),
            quote(&prefix(&first, 60))
        ),
    );
    change.notes.push(format!("tables after T{table_no} are renumbered by -{}", nested + 1));
    Ok((out, change))
}

// ---------------------------------------------------------------------------
// paragraphs

/// Where `insert-paragraph` puts its text.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Anchor {
    AfterTable(usize),
    BeforeTable(usize),
    /// After the paragraph that starts on this contents.xml line.
    AfterLine(usize),
}

/// The free paragraph (`P` outside any table cell) that starts on `line`.
fn paragraph_at(doc: &Doc, line: usize) -> Result<NodeId> {
    let tree = &doc.tree;
    let hits: Vec<NodeId> = tree
        .iter_tag(doc.root(), "P")
        .into_iter()
        .filter(|&p| doc.line(tree.node(p).start) == line)
        .filter(|&p| !tree.ancestors(p).iter().any(|&a| is_cell(tree.tag(a))))
        .collect();
    match hits.as_slice() {
        [one] => Ok(*one),
        [] => {
            let other = BLOCK_TEXT_TAGS
                .iter()
                .flat_map(|t| tree.iter_tag(doc.root(), t))
                .find(|&n| doc.line(tree.node(n).start) == line)
                .map(|n| format!(" (line {line} holds <{}>, not a free <P>)", tree.tag(n)));
            Err(Error::invalid(format!("no free paragraph starts on line {line}{}", other.unwrap_or_default()))
                .with_hint("take the line from `dsd read`; paragraphs inside table cells are edited with set-cell"))
        }
        _ => {
            Err(Error::refused(format!("{} paragraphs start on line {line}", hits.len()))
                .with_hint("edit the XML by hand"))
        }
    }
}

/// Insert a plain `<P>` paragraph; newlines in `text` become DART line breaks.
pub fn insert_paragraph(data: &[u8], text: &str, anchor: Anchor) -> Result<(Vec<u8>, Change)> {
    let doc = Doc::parse(data.to_vec())?;
    let (node, after, place) = match anchor {
        Anchor::AfterTable(t) => (verified_table(&doc, t)?.node, true, format!("after T{t}")),
        Anchor::BeforeTable(t) => (verified_table(&doc, t)?.node, false, format!("before T{t}")),
        Anchor::AfterLine(line) => {
            let p = paragraph_at(&doc, line)?;
            require_verified(&doc, p, &format!("the paragraph on line {line}"))?;
            (p, true, format!("after the paragraph on line {line}"))
        }
    };
    if doc.tree.ancestors(node).iter().any(|&a| is_cell(doc.tag(a))) {
        return Err(
            Error::refused(format!("the anchor {place} is inside a table cell")).with_hint("edit the XML by hand")
        );
    }
    // A form-managed TABLE-GROUP keeps its own content; the paragraph goes
    // beside the whole group instead.
    let group = doc.tree.ancestors(node).into_iter().find(|&a| doc.tag(a) == "TABLE-GROUP");
    let node = group.unwrap_or(node);
    let paragraph = format!("<P>{}</P>", xml_escape_text(text)).into_bytes();
    let gap = gap_before(&doc, node);
    let n = doc.tree.node(node);
    let edit = if after {
        (n.end, n.end, [&gap[..], &paragraph[..]].concat())
    } else {
        (n.start, n.start, [&paragraph[..], &gap[..]].concat())
    };
    let out = replace_ranges(data, vec![edit])?;
    let mut change =
        Change::new("insert-paragraph", format!("inserted a paragraph {place}: {}", quote(&prefix(text, 80))));
    if group.is_some() {
        change.notes.push(format!(
            "the anchor is in a TABLE-GROUP; the paragraph went {} the whole group",
            if after { "after" } else { "before" }
        ));
    }
    Ok((out, change))
}

/// Remove the free paragraph that starts on `line`.
pub fn delete_paragraph(data: &[u8], line: usize) -> Result<(Vec<u8>, Change)> {
    let doc = Doc::parse(data.to_vec())?;
    let p = paragraph_at(&doc, line)?;
    require_verified(&doc, p, &format!("the paragraph on line {line}"))?;
    if !doc.tree.iter_tag(p, "TABLE").is_empty() {
        return Err(
            Error::refused(format!("the paragraph on line {line} contains a table")).with_hint("edit the XML by hand")
        );
    }
    let text = node_text(&doc.tree, p, false);
    let (a, b) = removal(&doc, p);
    let out = replace_ranges(data, vec![(a, b, Vec::new())])?;
    Ok((
        out,
        Change::new("delete-paragraph", format!("deleted the paragraph on line {line}: {}", quote(&prefix(&text, 80)))),
    ))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn removes_attributes_with_either_quote() {
        assert_eq!(remove_attr_in_tag(b"<TD ROWSPAN=\"2\" WIDTH=\"9\">", "ROWSPAN"), b"<TD WIDTH=\"9\">");
        assert_eq!(remove_attr_in_tag(b"<TD X='1' ROWSPAN='3'>", "ROWSPAN"), b"<TD X='1'>");
        assert_eq!(remove_attr_in_tag(b"<TD WIDTH=\"9\">", "ROWSPAN"), b"<TD WIDTH=\"9\">");
    }

    #[test]
    fn spans_drop_at_one() {
        assert_eq!(with_span(b"<TD ROWSPAN=\"2\">", "ROWSPAN", 1), b"<TD>");
        assert_eq!(with_span(b"<TD>", "ROWSPAN", 3), b"<TD ROWSPAN=\"3\">");
    }

    #[test]
    fn total_widths_keep_their_relation_to_the_columns() {
        assert_eq!(total_width(600, 600, 3, 200, 1), 800);
        assert_eq!(total_width(627, 600, 3, 200, 1), 836);
        assert_eq!(total_width(600, 600, 3, 200, -1), 400);
    }
}
