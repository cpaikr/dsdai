//! Text extraction: normalized cell text, exact block text, and readable units.

use std::collections::BTreeSet;
use std::fmt::Write as _;

use crate::doc::{BLOCK_TEXT_TAGS, CR_TOKEN, Doc, OBSERVED_SCHEMAS, is_scaffold, is_verified};
use crate::error::{Error, RAW_ROUTE, Result};
use crate::xml::{Item, NodeId, Tree};

pub const LINE_BREAK_MARK: &str = "⏎";

/// Python's `str.strip()` set for the characters XML can carry.
pub fn is_py_space(c: char) -> bool {
    c.is_whitespace()
}

fn layout_free(text: &str) -> String {
    text.replace(['\r', '\n'], "")
}

/// Plain text with DART line breaks (`&cr;`) and block boundaries as `\n`.
///
/// Literal newlines in the XML are layout, not content, so they are dropped.
pub fn node_text(tree: &Tree, node: NodeId, include_scaffold: bool) -> String {
    const BOUNDARY: char = '\u{1e}';
    let mut joined = String::new();
    for item in &tree.node(node).items {
        match item {
            Item::Text(t) => joined.push_str(&layout_free(t)),
            Item::Node(child) => {
                let tag = tree.tag(*child);
                if is_scaffold(tag) && !include_scaffold {
                    continue;
                }
                if BLOCK_TEXT_TAGS.contains(&tag) {
                    joined.push(BOUNDARY);
                    joined.push_str(&node_text(tree, *child, include_scaffold));
                    joined.push(BOUNDARY);
                } else {
                    joined.push_str(&node_text(tree, *child, include_scaffold));
                }
            }
        }
    }
    // Adjacent blocks are separated by one newline; &cr; runs keep their blank lines.
    let joined = joined.replace(CR_TOKEN, "\n");
    let text = joined
        .split(BOUNDARY)
        .filter(|s| !s.is_empty() && !s.trim_matches([' ', '\t', '\u{3000}']).is_empty())
        .collect::<Vec<_>>()
        .join("\n");
    let mut lines: Vec<&str> = text.split('\n').map(|l| l.trim_end_matches(is_py_space)).collect();
    while lines.first().is_some_and(|l| l.trim_matches(is_py_space).is_empty()) {
        lines.remove(0);
    }
    while lines.last().is_some_and(|l| l.trim_matches(is_py_space).is_empty()) {
        lines.pop();
    }
    lines.join("\n")
}

/// Text with all whitespace removed, for whitespace-insensitive matching.
pub fn compact(text: &str) -> String {
    text.chars().filter(|c| !is_py_space(*c)).collect()
}

/// Whitespace runs collapsed to single spaces, trimmed (`" ".join(s.split())`).
pub fn collapse(text: &str) -> String {
    text.split_whitespace().collect::<Vec<_>>().join(" ")
}

/// The first `n` characters.
pub fn prefix(text: &str, n: usize) -> String {
    text.chars().take(n).collect()
}

/// JSON string quoting as the text views print it.
pub fn quote(text: &str) -> String {
    serde_json::to_string(text).expect("strings always serialize")
}

fn attr_value_label(v: &str) -> String {
    let plain = !v.is_empty() && v.chars().all(|c| !is_py_space(c) && !matches!(c, '[' | ']' | '"' | '='));
    if plain { v.to_owned() } else { quote(v) }
}

/// `[K=V ...]` for the node's attributes (all, or only `names`).
pub fn attr_label(tree: &Tree, node: NodeId, names: Option<&[&str]>) -> String {
    let items: Vec<String> = tree
        .node(node)
        .attrs
        .iter()
        .filter(|(k, _)| names.is_none_or(|ns| ns.contains(&k.as_str())))
        .map(|(k, v)| format!("{k}={}", attr_value_label(v)))
        .collect();
    if items.is_empty() { String::new() } else { format!("[{}]", items.join(" ")) }
}

pub const INLINE_TAGS: [&str; 2] = ["SPAN", "A"];

/// A block's text without whitespace normalization.
///
/// `&cr;` becomes the line-break mark. SPAN/A text joins in place; any other
/// child element stays visible as `⟦TAG[attrs]⟧…⟦/TAG⟧`, and a table as a
/// `⟦T#⟧` stub so its cells keep their `table` coordinates.
pub fn exact_text(doc: &Doc, node: NodeId, scaffold: bool, with_tables: bool, mark: &str) -> String {
    let tree = &doc.tree;
    let mut out = String::new();
    for item in &tree.node(node).items {
        match item {
            Item::Text(t) => out.push_str(&layout_free(t).replace(CR_TOKEN, mark)),
            Item::Node(child) => {
                let tag = tree.tag(*child);
                if is_scaffold(tag) && !scaffold {
                    continue;
                }
                if INLINE_TAGS.contains(&tag) {
                    out.push_str(&exact_text(doc, *child, scaffold, with_tables, mark));
                } else if let Some(t) = doc.table_for_node(*child).filter(|_| with_tables) {
                    let _ = write!(out, "⟦T{} -> table {}⟧", t.index, t.index);
                } else {
                    let _ = write!(
                        out,
                        "⟦{tag}{}⟧{}⟦/{tag}⟧",
                        attr_label(tree, *child, None),
                        exact_text(doc, *child, scaffold, with_tables, mark)
                    );
                }
            }
        }
    }
    out
}

/// Elements in the subtree (itself included) outside the verified set.
/// Editor scaffolding is skipped: nothing inside it is interpreted.
pub fn unverified_nodes(tree: &Tree, node: NodeId) -> Vec<NodeId> {
    let mut out = Vec::new();
    let mut stack = vec![node];
    while let Some(n) = stack.pop() {
        let tag = tree.tag(n);
        if is_scaffold(tag) {
            continue;
        }
        if !is_verified(tag) {
            out.push(n);
        }
        stack.extend(tree.children(n).iter().rev());
    }
    out
}

pub fn unverified_tags(tree: &Tree, node: NodeId) -> Vec<String> {
    unverified_nodes(tree, node)
        .into_iter()
        .map(|n| tree.tag(n).to_owned())
        .collect::<BTreeSet<_>>()
        .into_iter()
        .collect()
}

pub fn unverified_ancestor_tags(tree: &Tree, node: NodeId) -> Vec<String> {
    tree.ancestors(node)
        .into_iter()
        .map(|a| tree.tag(a).to_owned())
        .filter(|t| !is_verified(t))
        .collect::<BTreeSet<_>>()
        .into_iter()
        .collect()
}

#[derive(Debug, Clone, Default)]
pub struct HeaderInfo {
    pub schema: String,
    pub document_name: Option<String>,
    pub document_code: Option<String>,
    pub formula_version: Option<String>,
    pub formula_date: Option<String>,
}

pub fn header_info(doc: &Doc) -> HeaderInfo {
    let tree = &doc.tree;
    let root = tree.node(doc.root());
    let schema =
        root.attrs.iter().filter(|(k, _)| k.ends_with("noNamespaceSchemaLocation")).map(|(_, v)| v.clone()).next_back();
    let mut info = HeaderInfo { schema: schema.unwrap_or_default(), ..HeaderInfo::default() };
    if let Some(n) = tree.first_tag(doc.root(), "DOCUMENT-NAME") {
        info.document_name = Some(node_text(tree, n, false));
        info.document_code = Some(tree.node(n).attr("ACODE").unwrap_or("").to_owned());
    }
    if let Some(n) = tree.first_tag(doc.root(), "FORMULA-VERSION") {
        info.formula_version = Some(node_text(tree, n, false));
        info.formula_date = Some(tree.node(n).attr("ADATE").unwrap_or("").to_owned());
    }
    info
}

/// The declared schema when it is not one the tool was verified on.
pub fn unverified_schema(doc: &Doc) -> Option<String> {
    let schema = header_info(doc).schema;
    if OBSERVED_SCHEMAS.contains(&schema.to_lowercase().as_str()) {
        None
    } else if schema.is_empty() {
        Some("(none declared)".to_owned())
    } else {
        Some(schema)
    }
}

/// Refuse an edit whose interpretation depends on unverified structure.
pub fn require_verified(doc: &Doc, node: NodeId, place: &str) -> Result<()> {
    let unverified = |message: String| Error::new(crate::ErrorCode::UnverifiedContent, message).with_hint(RAW_ROUTE);
    if let Some(schema) = unverified_schema(doc) {
        return Err(unverified(format!("document schema {schema} is not one this tool was verified on")));
    }
    let names = |tags: &[String]| tags.iter().map(|t| format!("<{t}>")).collect::<Vec<_>>().join(", ");
    let inner = unverified_tags(&doc.tree, node);
    if !inner.is_empty() {
        return Err(unverified(format!("{place} contains {}, which this tool has not verified", names(&inner))));
    }
    // An unverified container may change what the target means, so its ancestors count too.
    let outer = unverified_ancestor_tags(&doc.tree, node);
    if !outer.is_empty() {
        return Err(unverified(format!("{place} lies inside {}, which this tool has not verified", names(&outer))));
    }
    Ok(())
}

/// A readable unit: a table cell or a free text block.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Block {
    pub locator: String,
    pub tag: String,
    pub text: String,
    pub line: usize,
    /// Attributes sorted by name.
    pub attrs: Vec<(String, String)>,
    /// Unverified tags in or around this block.
    pub unverified: Vec<String>,
    /// Cells of one table row share a group; a text block is its own group.
    pub group: String,
}

impl Block {
    pub fn key(&self) -> (&str, &str, &[(String, String)]) {
        (&self.tag, &self.text, &self.attrs)
    }
}

fn sorted_attrs(tree: &Tree, node: NodeId) -> Vec<(String, String)> {
    let mut attrs = tree.node(node).attrs.clone();
    attrs.sort();
    attrs
}

fn union(outer: &[String], inner: Vec<String>) -> Vec<String> {
    outer.iter().cloned().chain(inner).collect::<BTreeSet<_>>().into_iter().collect()
}

/// Readable units in document order: table cells and free text blocks.
pub fn blocks(doc: &Doc) -> Vec<Block> {
    fn walk(doc: &Doc, node: NodeId, outer: &[String], out: &mut Vec<Block>) {
        let tree = &doc.tree;
        for &child in tree.children(node) {
            let tag = tree.tag(child);
            if is_scaffold(tag) || tag == "DOCUMENT-HEADER" {
                continue;
            }
            if let Some(table) = doc.table_for_node(child) {
                let around = union(outer, unverified_tags(tree, child));
                for cell in &table.cells {
                    out.push(Block {
                        locator: format!("T{}R{}C{}", table.index, cell.row, cell.col),
                        tag: tree.tag(cell.node).to_owned(),
                        text: node_text(tree, cell.node, false),
                        line: doc.line(tree.node(cell.node).start),
                        attrs: sorted_attrs(tree, cell.node),
                        unverified: around.clone(),
                        group: format!("T{}R{}", table.index, cell.row),
                    });
                }
            } else if BLOCK_TEXT_TAGS.contains(&tag) {
                let start = tree.node(child).start;
                out.push(Block {
                    locator: format!("L{}", doc.line(start)),
                    tag: tag.to_owned(),
                    text: node_text(tree, child, false),
                    line: doc.line(start),
                    attrs: sorted_attrs(tree, child),
                    unverified: union(outer, unverified_tags(tree, child)),
                    // Byte offset: unique even when blocks share a line.
                    group: format!("@{start}"),
                });
            } else {
                let inner = if is_verified(tag) { outer.to_vec() } else { union(outer, vec![tag.to_owned()]) };
                walk(doc, child, &inner, out);
            }
        }
    }
    let mut out = Vec::new();
    walk(doc, doc.root(), &[], &mut out);
    out
}
