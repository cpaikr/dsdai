//! Edits as exact byte-range replacements on `contents.xml`.
//!
//! Nothing is re-serialized: each operation locates its target in the parsed
//! tree and splices new bytes into the original, so every byte outside the
//! change stays as DART Editor wrote it.

use std::collections::BTreeMap;
use std::path::Path;

use serde::Serialize;
use serde_json::Value;

use crate::archive::{Archive, CONTENTS, archive_bytes, atomic_write, guard_output, read_archive};
use crate::check::{Issue, Level, check_archive, line_ending_issue, unit_value_conflict};
use crate::doc::{Doc, is_scaffold, is_verified};
use crate::error::{Error, ErrorCode, RAW_ROUTE, Result};
use crate::structure;
use crate::text::{node_text, quote, require_verified, unverified_nodes, unverified_schema};
use crate::views::leaf_for_text;
use crate::xml::NodeId;

/// One applied operation, for reports.
#[derive(Debug, Clone, Serialize)]
pub struct Change {
    pub op: &'static str,
    pub summary: String,
    #[serde(skip_serializing_if = "Vec::is_empty")]
    pub notes: Vec<String>,
}

impl Change {
    pub(crate) fn new(op: &'static str, summary: String) -> Self {
        Self { op, summary, notes: Vec::new() }
    }
}

pub(crate) type Splice = (usize, usize, Vec<u8>);

/// Escape text for character data; newlines become DART's `&cr;` token.
pub fn xml_escape_text(text: &str) -> String {
    let text = text.replace("\r\n", "\n").replace('\r', "\n");
    let escaped = text.replace('&', "&amp;").replace('<', "&lt;").replace('>', "&gt;").replace('"', "&quot;");
    escaped.replace('\n', "&amp;cr;")
}

pub(crate) fn replace_ranges(data: &[u8], mut edits: Vec<Splice>) -> Result<Vec<u8>> {
    edits.sort_by_key(|e| e.0);
    if edits.windows(2).any(|w| w[1].0 < w[0].1) {
        return Err(Error::invalid("internal error: overlapping edits"));
    }
    let mut out = Vec::with_capacity(data.len());
    let mut pos = 0;
    for (a, b, new) in edits {
        out.extend_from_slice(&data[pos..a]);
        out.extend_from_slice(&new);
        pos = b;
    }
    out.extend_from_slice(&data[pos..]);
    Ok(out)
}

pub(crate) fn is_ascii_space(b: u8) -> bool {
    matches!(b, b' ' | b'\t' | b'\n' | b'\r' | 0x0b | 0x0c)
}

fn trim_ascii_end(bytes: &[u8]) -> &[u8] {
    let end = bytes.iter().rposition(|&b| !is_ascii_space(b)).map_or(0, |p| p + 1);
    &bytes[..end]
}

pub(crate) fn is_blank(bytes: &[u8]) -> bool {
    bytes.iter().all(|&b| is_ascii_space(b))
}

/// Set (or add) a double-quoted attribute inside the start tag `tag`.
pub(crate) fn set_attr_in_tag(tag: &[u8], name: &str, value: &str) -> Vec<u8> {
    let escaped = value.replace('&', "&amp;").replace('"', "&quot;").replace('<', "&lt;");
    let name = name.as_bytes();
    // First ` NAME = "value"` occurrence: whitespace, name, optional spaces, =, optional spaces, quote.
    let mut i = 0;
    while i < tag.len() {
        if is_ascii_space(tag[i]) && tag[i + 1..].starts_with(name) {
            let mut j = i + 1 + name.len();
            while j < tag.len() && is_ascii_space(tag[j]) {
                j += 1;
            }
            if tag.get(j) == Some(&b'=') {
                j += 1;
                while j < tag.len() && is_ascii_space(tag[j]) {
                    j += 1;
                }
                if let Some(&quote @ (b'"' | b'\'')) = tag.get(j)
                    && let Some(close) = tag[j + 1..].iter().position(|&b| b == quote)
                {
                    let (vs, ve) = (j + 1, j + 1 + close);
                    let value = if quote == b'\'' { escaped.replace('\'', "&apos;") } else { escaped };
                    return [&tag[..vs], value.as_bytes(), &tag[ve..]].concat();
                }
            }
        }
        i += 1;
    }
    let insert_at = tag.len() - if tag.ends_with(b"/>") { 2 } else { 1 };
    [&tag[..insert_at], b" ", name, b"=\"", escaped.as_bytes(), b"\"", &tag[insert_at..]].concat()
}

pub(crate) fn set_attr(data: &[u8], doc: &Doc, node: NodeId, name: &str, value: &str) -> Splice {
    let n = doc.tree.node(node);
    (n.start, n.open_end, set_attr_in_tag(&data[n.start..n.open_end], name, value))
}

/// Replace one cell's text, keeping its wrapper; refuse ambiguous or unit-inconsistent edits.
pub fn set_cell(
    data: &[u8],
    table_no: usize,
    row: usize,
    col: usize,
    text: &str,
    unit_value: Option<&str>,
    flatten: bool,
) -> Result<(Vec<u8>, Change)> {
    let doc = Doc::parse(data.to_vec())?;
    let table = doc.table(table_no)?;
    // Coordinates come from the whole table's structure, so check all of it.
    require_verified(&doc, table.node, &format!("T{table_no}"))?;
    let cell = doc.cell_at(table, row, col)?;
    if (cell.row, cell.col) != (row, col) {
        // Writing the anchor instead would silently change a header or label the caller never addressed.
        return Err(Error::refused(format!(
            "T{table_no} R{row}C{col} is covered by the spanning cell at R{}C{}",
            cell.row, cell.col
        ))
        .with_hint(format!("address that cell (--row {} --col {}) if it is the one to change", cell.row, cell.col)));
    }
    let node = cell.node;
    let tree = &doc.tree;
    let n = tree.node(node);
    let place = format!("T{table_no}R{row}C{col}");
    if n.tag == "TU"
        && let Some(current) = n.attr("AUNITVALUE").filter(|v| !v.is_empty())
        && unit_value.is_none()
    {
        return Err(Error::refused(format!(
            "{place} is a TU cell with AUNITVALUE={} (AUNIT={})",
            quote(current),
            n.attr("AUNIT").unwrap_or("")
        ))
        .with_hint("pass the matching machine value with --unit-value"));
    }
    if let Some(value) = unit_value {
        if n.tag != "TU" {
            return Err(Error::invalid(format!("--unit-value applies only to TU cells; {place} is {}", n.tag)));
        }
        if let Some(conflict) = unit_value_conflict(n.attr("AUNIT").unwrap_or(""), text, value) {
            return Err(
                Error::refused(format!("{place}: {conflict}")).with_hint("pass a --unit-value that agrees with --text")
            );
        }
    }
    let leaf = match leaf_for_text(&doc, node) {
        Some(leaf) => leaf,
        None if flatten => node,
        None => {
            return Err(Error::refused(format!("{place} holds mixed or multi-paragraph content"))
                .with_hint("edit contents.xml directly, or pass --flatten to replace it with plain text"));
        }
    };
    let new_inner = xml_escape_text(text).into_bytes();
    let mut edits: Vec<Splice> = Vec::new();
    if let Some(value) = unit_value
        && leaf != node
    {
        edits.push(set_attr(data, &doc, node, "AUNITVALUE", value));
    }
    let l = tree.node(leaf);
    if l.empty_tag {
        // Written as <P/> or <TD .../>: expand into a start/end pair.
        let mut open = trim_ascii_end(&data[l.start..l.open_end - 2]).to_vec();
        open.push(b'>');
        if let Some(value) = unit_value
            && leaf == node
        {
            open = set_attr_in_tag(&open, "AUNITVALUE", value);
        }
        let mut replacement = open;
        replacement.extend_from_slice(&new_inner);
        replacement.extend_from_slice(format!("</{}>", l.tag).as_bytes());
        edits.push((l.start, l.end, replacement));
    } else {
        if let Some(value) = unit_value
            && leaf == node
        {
            edits.push(set_attr(data, &doc, node, "AUNITVALUE", value));
        }
        edits.push((l.open_end, l.close_start, new_inner));
    }
    let before = node_text(tree, node, false);
    let out = replace_ranges(data, edits)?;
    Ok((out, Change::new("set-cell", format!("{place} {}: {} -> {}", n.tag, quote(&before), quote(text)))))
}

/// Byte ranges of character data directly inside elements of `node`.
///
/// Ranges come from the parsed tree, so tags and attribute values are never
/// included; editor scaffolding is skipped.
pub(crate) fn text_runs(doc: &Doc, node: NodeId, out: &mut Vec<(usize, usize)>) {
    let tree = &doc.tree;
    let n = tree.node(node);
    if is_scaffold(&n.tag) || n.close_start == n.open_end {
        return;
    }
    let mut pos = n.open_end;
    for &k in &n.children {
        out.push((pos, tree.node(k).start));
        pos = tree.node(k).end;
    }
    out.push((pos, n.close_start));
    for &k in &n.children {
        text_runs(doc, k, out);
    }
}

/// A decoded run: one char per logical character, with its source byte span.
/// `\0` marks a barrier (a comment, processing instruction, CDATA section, or
/// layout newline) that a match never crosses.
fn decode_run(data: &[u8], a: usize, b: usize) -> (Vec<char>, Vec<(usize, usize)>) {
    let mut chars = Vec::new();
    let mut spans = Vec::new();
    let mut pos = a;
    while pos < b {
        let rest = &data[pos..b];
        let non_text_end = if rest.starts_with(b"<!--") {
            find(rest, b"-->", 4).map(|e| e + 3)
        } else if rest.starts_with(b"<?") {
            find(rest, b"?>", 2).map(|e| e + 2)
        } else if rest.starts_with(b"<![CDATA[") {
            find(rest, b"]]>", 9).map(|e| e + 3)
        } else {
            None
        };
        if let Some(len) = non_text_end {
            chars.push('\0');
            spans.push((pos, pos + len));
            pos += len;
            continue;
        }
        let (ch, len) = decode_unit(rest);
        if len == 0 {
            pos += 1; // A stray byte no unit pattern covers.
            continue;
        }
        chars.push(ch);
        spans.push((pos, pos + len));
        pos += len;
    }
    (chars, spans)
}

fn find(hay: &[u8], needle: &[u8], from: usize) -> Option<usize> {
    hay.get(from..)?.windows(needle.len()).position(|w| w == needle).map(|p| p + from)
}

/// One text unit at the start of `rest`: `&amp;cr;`, a reference, or a character.
fn decode_unit(rest: &[u8]) -> (char, usize) {
    if rest.starts_with(b"&amp;cr;") {
        return ('\n', 8);
    }
    if rest[0] == b'&' {
        if let Some(len) = reference_len(rest) {
            let body = &rest[1..len - 1];
            let text = std::str::from_utf8(body).unwrap_or("");
            let ch = if let Some(num) = text.strip_prefix('#') {
                let parsed = match num.strip_prefix(['x', 'X']) {
                    Some(hex) => u32::from_str_radix(hex, 16).ok(),
                    None => num.parse::<u32>().ok(),
                };
                parsed.and_then(char::from_u32).unwrap_or('\u{fffd}')
            } else {
                match text {
                    "amp" => '&',
                    "lt" => '<',
                    "gt" => '>',
                    "quot" => '"',
                    "apos" => '\'',
                    // Unknown names cannot occur in a well-formed document.
                    _ => '\u{fffd}',
                }
            };
            return (ch, len);
        }
        return ('&', 1);
    }
    if rest[0] < 0x80 {
        let ch = char::from(rest[0]);
        // XML layout newline, not content.
        return (if ch == '\r' || ch == '\n' { '\0' } else { ch }, 1);
    }
    if (0xc0..=0xff).contains(&rest[0]) {
        let len = 1 + rest[1..].iter().take_while(|&&b| (0x80..0xc0).contains(&b)).count();
        let ch = std::str::from_utf8(&rest[..len]).ok().and_then(|s| s.chars().next()).unwrap_or('\u{fffd}');
        return (ch, len);
    }
    ('\0', 0)
}

/// Length of `&name;`, `&#digits;`, or `&#xhex;` at the start of `rest`.
fn reference_len(rest: &[u8]) -> Option<usize> {
    let body_end = rest[1..].iter().position(|&b| b == b';')? + 1;
    let body = &rest[1..body_end];
    let ok = if let Some(num) = body.strip_prefix(b"#") {
        let digits = num.strip_prefix(b"x").unwrap_or(num);
        !digits.is_empty() && digits.iter().all(u8::is_ascii_hexdigit)
    } else {
        !body.is_empty() && body.iter().all(u8::is_ascii_alphabetic)
    };
    ok.then_some(body_end + 1)
}

fn chars_of(text: &str) -> Vec<char> {
    text.replace("\r\n", "\n").replace('\r', "\n").chars().collect()
}

/// Start positions of `needle` in `chars`, non-overlapping, left to right.
fn positions(chars: &[char], needle: &[char]) -> Vec<usize> {
    let mut out = Vec::new();
    let mut pos = 0;
    while !needle.is_empty() && pos + needle.len() <= chars.len() {
        if chars[pos..pos + needle.len()] == needle[..] {
            out.push(pos);
            pos += needle.len();
        } else {
            pos += 1;
        }
    }
    out
}

/// Byte spans of `old` in the text runs under `root`, plus how many hits were
/// skipped because they lie inside an occurrence of an `except` phrase.
fn text_hits(doc: &Doc, old: &str, root: NodeId, except: &[String]) -> (Vec<(usize, usize)>, usize) {
    let needle = chars_of(old);
    let guards: Vec<Vec<char>> = except.iter().map(|e| chars_of(e)).filter(|e| !e.is_empty()).collect();
    let mut runs = Vec::new();
    text_runs(doc, root, &mut runs);
    let (mut hits, mut skipped) = (Vec::new(), 0);
    for (a, b) in runs {
        let (chars, spans) = decode_run(&doc.data, a, b);
        let protected: Vec<(usize, usize)> =
            guards.iter().flat_map(|g| positions(&chars, g).into_iter().map(move |p| (p, p + g.len()))).collect();
        for pos in positions(&chars, &needle) {
            if protected.iter().any(|&(s, e)| s <= pos && pos + needle.len() <= e) {
                skipped += 1;
            } else {
                hits.push((spans[pos].0, spans[pos + needle.len() - 1].1));
            }
        }
    }
    (hits, skipped)
}

#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct Occurrence {
    pub line: usize,
    pub context: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub unit_value: Option<String>,
    #[serde(skip_serializing_if = "Vec::is_empty")]
    pub unverified: Vec<String>,
}

#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct ReplacePreview {
    pub old: String,
    pub scope: String,
    pub count: usize,
    /// Occurrences left alone because they lie inside an `except` phrase.
    #[serde(skip_serializing_if = "is_zero")]
    pub skipped: usize,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub unverified_schema: Option<String>,
    pub occurrences: Vec<Occurrence>,
}

#[allow(clippy::trivially_copy_pass_by_ref)]
fn is_zero(v: &usize) -> bool {
    *v == 0
}

/// Lossy UTF-8 that drops broken sequences, as at a cut-off context window.
fn decode_ignore(bytes: &[u8]) -> String {
    bytes.utf8_chunks().map(|c| c.valid()).collect()
}

pub enum ReplaceOutcome {
    Preview(ReplacePreview),
    Edited(Vec<u8>, Change),
}

/// What `replace` changes and how it is guarded.
#[derive(Debug, Clone, Copy, Default)]
pub struct ReplaceSpec<'a> {
    pub old: &'a str,
    pub new: &'a str,
    /// Required number of replaced occurrences; `None` accepts any.
    pub expect: Option<usize>,
    pub table: Option<usize>,
    /// Phrases whose occurrences keep any `old` inside them (분ㆍ반기 when 반기 becomes 분기).
    pub except: &'a [String],
    pub dry_run: bool,
}

/// Replace literal text inside text runs, guarded by an expected count.
pub fn replace(data: &[u8], spec: &ReplaceSpec<'_>) -> Result<ReplaceOutcome> {
    let ReplaceSpec { old, new, expect, table: table_no, except, dry_run } = *spec;
    if old.is_empty() {
        return Err(Error::invalid("--old must not be empty"));
    }
    let doc = Doc::parse(data.to_vec())?;
    let tree = &doc.tree;
    let (root, scope) = match table_no {
        Some(t) => (doc.table(t)?.node, format!("T{t}")),
        None => (tree.first_tag(doc.root(), "BODY").unwrap_or(doc.root()), "document body".to_owned()),
    };
    let repl = xml_escape_text(new).into_bytes();
    let (hits, skipped) = text_hits(&doc, old, root, except);
    let unit_cells: Vec<NodeId> = doc
        .tables
        .iter()
        .flat_map(|t| t.cells.iter())
        .filter(|c| tree.tag(c.node) == "TU" && tree.node(c.node).attr("AUNITVALUE").is_some_and(|v| !v.is_empty()))
        .map(|c| c.node)
        .collect();
    let unit_note = |offset: usize| {
        unit_cells
            .iter()
            .find(|&&c| tree.node(c).open_end <= offset && offset < tree.node(c).close_start)
            .map(|&c| tree.node(c).attr("AUNITVALUE").unwrap_or("").to_owned())
    };
    // With --table the root is the TABLE; an unverified container around it encloses every hit.
    let mut unverified = unverified_nodes(tree, root);
    unverified.extend(tree.ancestors(root).into_iter().filter(|&a| !is_verified(tree.tag(a))));
    let unverified_at = |offset: usize| {
        let mut tags: Vec<String> = unverified
            .iter()
            .filter(|&&n| tree.node(n).open_end <= offset && offset < tree.node(n).close_start)
            .map(|&n| tree.tag(n).to_owned())
            .collect();
        tags.sort();
        tags.dedup();
        tags
    };
    let schema = unverified_schema(&doc);
    if dry_run {
        let occurrences = hits
            .iter()
            .take(200)
            .map(|&(a, b)| {
                let (lo, hi) = (a.saturating_sub(40), (b + 40).min(data.len()));
                let context = decode_ignore(&data[lo..hi]).replace('\r', "").replace('\n', "⏎");
                Occurrence { line: doc.line(a), context, unit_value: unit_note(a), unverified: unverified_at(a) }
            })
            .collect();
        return Ok(ReplaceOutcome::Preview(ReplacePreview {
            old: old.to_owned(),
            scope,
            count: hits.len(),
            skipped,
            unverified_schema: schema,
            occurrences,
        }));
    }
    if hits.is_empty() {
        return Err(Error::new(ErrorCode::TextNotFound, format!("{} not found in text of {scope}", quote(old)))
            .with_hint("check spacing with `dsd find` (attribute values and tags are never matched)"));
    }
    // Refuse unverified targets before the count check, so fixing --expect is never a wasted round.
    if let Some(schema) = schema {
        return Err(Error::new(
            ErrorCode::UnverifiedContent,
            format!("document schema {schema} is not one this tool was verified on"),
        )
        .with_hint(RAW_ROUTE));
    }
    let inside: Vec<String> =
        hits.iter().filter(|(a, _)| !unverified_at(*a).is_empty()).map(|(a, _)| doc.line(*a).to_string()).collect();
    if !inside.is_empty() {
        return Err(Error::new(
            ErrorCode::UnverifiedContent,
            format!(
                "{} occurrence(s) lie inside elements this tool has not verified (lines {})",
                inside.len(),
                inside.iter().take(15).cloned().collect::<Vec<_>>().join(", ")
            ),
        )
        .with_hint(RAW_ROUTE));
    }
    if let Some(expect) = expect
        && hits.len() != expect
    {
        let lines: Vec<String> = hits.iter().take(15).map(|h| doc.line(h.0).to_string()).collect();
        return Err(Error::new(
            ErrorCode::CountMismatch,
            format!(
                "found {} occurrence(s) of {} in {scope}, expected {expect} (lines {})",
                hits.len(),
                quote(old),
                lines.join(", ")
            ),
        )
        .with_hint("narrow with --table, or set --expect to the verified count"));
    }
    let mut lines: Vec<usize> = hits.iter().map(|h| doc.line(h.0)).collect();
    lines.sort_unstable();
    lines.dedup();
    let shown = lines.iter().take(20).map(ToString::to_string).collect::<Vec<_>>().join(", ");
    let mut change = Change::new(
        "replace",
        format!("replaced {} occurrence(s) of {} with {} at lines {shown}", hits.len(), quote(old), quote(new)),
    );
    if skipped > 0 {
        change.notes.push(format!("{skipped} occurrence(s) inside an except phrase were left as is"));
    }
    let tu_hits = hits.iter().filter(|(a, _)| unit_note(*a).is_some()).count();
    if tu_hits > 0 {
        change.notes.push(format!(
            "{tu_hits} occurrence(s) were in TU cells; their AUNITVALUE was left as is (use set-cell --unit-value)"
        ));
    }
    let edits = hits.into_iter().map(|(a, b)| (a, b, repl.clone())).collect();
    Ok(ReplaceOutcome::Edited(replace_ranges(data, edits)?, change))
}

// ---------------------------------------------------------------------------
// apply

#[derive(Clone, Copy)]
enum FieldType {
    Int,
    Str,
    Bool,
    Expect,
    StrList,
}

struct Field {
    name: &'static str,
    kind: FieldType,
    required: bool,
}

const fn field(name: &'static str, kind: FieldType, required: bool) -> Field {
    Field { name, kind, required }
}

pub struct OpSpec {
    pub op: &'static str,
    fields: &'static [Field],
}

pub const OPS: [OpSpec; 10] = [
    OpSpec {
        op: "set-cell",
        fields: &[
            field("table", FieldType::Int, true),
            field("row", FieldType::Int, true),
            field("col", FieldType::Int, true),
            field("text", FieldType::Str, true),
            field("unit_value", FieldType::Str, false),
            field("flatten", FieldType::Bool, false),
        ],
    },
    OpSpec {
        op: "replace",
        fields: &[
            field("old", FieldType::Str, true),
            field("new", FieldType::Str, true),
            field("expect", FieldType::Expect, false),
            field("table", FieldType::Int, false),
            field("except", FieldType::StrList, false),
        ],
    },
    OpSpec {
        op: "copy-row",
        fields: &[
            field("table", FieldType::Int, true),
            field("row", FieldType::Int, true),
            field("count", FieldType::Int, false),
            field("clear", FieldType::Bool, false),
            field("force_locked", FieldType::Bool, false),
        ],
    },
    OpSpec {
        op: "delete-row",
        fields: &[
            field("table", FieldType::Int, true),
            field("row", FieldType::Int, true),
            field("force_locked", FieldType::Bool, false),
        ],
    },
    OpSpec {
        op: "copy-col",
        fields: &[
            field("table", FieldType::Int, true),
            field("col", FieldType::Int, true),
            field("count", FieldType::Int, false),
            field("clear", FieldType::Bool, false),
            field("force_locked", FieldType::Bool, false),
        ],
    },
    OpSpec {
        op: "delete-col",
        fields: &[
            field("table", FieldType::Int, true),
            field("col", FieldType::Int, true),
            field("force_locked", FieldType::Bool, false),
        ],
    },
    OpSpec {
        op: "copy-table",
        fields: &[
            field("table", FieldType::Int, true),
            field("count", FieldType::Int, false),
            field("clear", FieldType::Bool, false),
            field("force_locked", FieldType::Bool, false),
        ],
    },
    OpSpec {
        op: "delete-table",
        fields: &[field("table", FieldType::Int, true), field("force_locked", FieldType::Bool, false)],
    },
    OpSpec {
        op: "insert-paragraph",
        fields: &[
            field("text", FieldType::Str, true),
            field("after_table", FieldType::Int, false),
            field("before_table", FieldType::Int, false),
            field("after_line", FieldType::Int, false),
        ],
    },
    OpSpec { op: "delete-paragraph", fields: &[field("line", FieldType::Int, true)] },
];

/// `name: type` listing of each operation's fields; `?` marks optional ones.
pub fn op_reference() -> Vec<String> {
    OPS.iter()
        .map(|spec| {
            let fields = spec
                .fields
                .iter()
                .map(|f| {
                    let kind = match f.kind {
                        FieldType::Int => "int",
                        FieldType::Str => "str",
                        FieldType::Bool => "bool",
                        FieldType::Expect => "int|\"any\"",
                        FieldType::StrList => "[str]",
                    };
                    format!("{}{}: {kind}", f.name, if f.required { "" } else { "?" })
                })
                .collect::<Vec<_>>()
                .join(", ");
            format!("{:10} {fields}", spec.op)
        })
        .collect()
}

/// Parse an expected count: a positive integer, or `any` (or null) for no guard.
pub fn parse_expect(value: &Value) -> Result<Option<usize>> {
    let bad = || Error::invalid(format!("expect must be a positive integer or 'any', got {value}"));
    let number = match value {
        Value::Null => return Ok(None),
        Value::String(s) if s == "any" => return Ok(None),
        Value::String(s) => s.trim().parse::<i64>().map_err(|_| bad())?,
        Value::Number(n) => n.as_i64().or_else(|| n.as_f64().map(|f| f.trunc() as i64)).ok_or_else(bad)?,
        _ => return Err(bad()),
    };
    usize::try_from(number).ok().filter(|&n| n >= 1).map(Some).ok_or_else(bad)
}

fn as_index(op: &serde_json::Map<String, Value>, name: &str) -> Result<Option<usize>> {
    match op.get(name) {
        None | Some(Value::Null) => Ok(None),
        Some(v) => {
            let n = v.as_i64().ok_or_else(|| Error::invalid(format!("field {name:?} must be an integer, got {v}")))?;
            usize::try_from(n)
                .map(Some)
                .map_err(|_| Error::invalid(format!("field {name:?} must not be negative, got {n}")))
        }
    }
}

fn validate(op: &Value) -> Result<(&'static OpSpec, &serde_json::Map<String, Value>)> {
    let obj = op.as_object().ok_or_else(|| Error::invalid("each edit must be a JSON object"))?;
    let kind = obj.get("op").and_then(Value::as_str).unwrap_or("");
    let spec = OPS.iter().find(|s| s.op == kind).ok_or_else(|| {
        Error::invalid(format!(
            "unknown op {}; use {}",
            obj.get("op").map_or_else(|| "(missing)".to_owned(), ToString::to_string),
            OPS.iter().map(|s| s.op).collect::<Vec<_>>().join(", ")
        ))
    })?;
    let mut unknown: Vec<&String> =
        obj.keys().filter(|k| *k != "op" && !spec.fields.iter().any(|f| f.name == k.as_str())).collect();
    unknown.sort();
    if !unknown.is_empty() {
        let accepted = spec.fields.iter().map(|f| f.name).collect::<Vec<_>>().join(", ");
        return Err(Error::invalid(format!("unknown field(s) {unknown:?}; {kind} accepts {accepted}")));
    }
    for f in spec.fields {
        let value = obj.get(f.name).filter(|v| !v.is_null());
        let Some(value) = value else {
            if f.required {
                return Err(Error::invalid(format!("missing field {:?}", f.name)));
            }
            continue;
        };
        let ok = match f.kind {
            FieldType::Int => value.is_i64() || value.is_u64(),
            FieldType::Str => value.is_string(),
            FieldType::Bool => value.is_boolean(),
            FieldType::Expect => true,
            FieldType::StrList => value.as_array().is_some_and(|a| a.iter().all(Value::is_string)),
        };
        if !ok {
            let want = match f.kind {
                FieldType::Int => "an integer",
                FieldType::Str => "a string",
                FieldType::StrList => "an array of strings",
                _ => "true or false",
            };
            return Err(Error::invalid(format!("field {:?} must be {want}, got {value}", f.name)));
        }
    }
    Ok((spec, obj))
}

/// Exactly one of the three anchors `insert-paragraph` accepts.
pub fn paragraph_anchor(
    after_table: Option<usize>,
    before_table: Option<usize>,
    after_line: Option<usize>,
) -> Result<structure::Anchor> {
    match (after_table, before_table, after_line) {
        (Some(t), None, None) => Ok(structure::Anchor::AfterTable(t)),
        (None, Some(t), None) => Ok(structure::Anchor::BeforeTable(t)),
        (None, None, Some(l)) => Ok(structure::Anchor::AfterLine(l)),
        _ => Err(Error::invalid("give exactly one of after_table, before_table, after_line")),
    }
}

/// Run a JSON array of edits in order against `contents.xml`.
pub fn apply(data: &[u8], ops: &[Value]) -> Result<(Vec<u8>, Vec<Change>)> {
    let mut data = data.to_vec();
    let mut log = Vec::new();
    for (i, raw) in ops.iter().enumerate() {
        let kind = raw.get("op").and_then(Value::as_str).unwrap_or("?").to_owned();
        let step = || -> Result<(Vec<u8>, Change)> {
            let (spec, op) = validate(raw)?;
            let text = |name: &str| op.get(name).and_then(Value::as_str);
            let flag = |name: &str| op.get(name).and_then(Value::as_bool).unwrap_or(false);
            let req = |name: &str| as_index(op, name).map(|v| v.expect("required fields are present"));
            match spec.op {
                "set-cell" => set_cell(
                    &data,
                    req("table")?,
                    req("row")?,
                    req("col")?,
                    text("text").unwrap_or(""),
                    text("unit_value"),
                    flag("flatten"),
                ),
                "replace" => {
                    let expect = parse_expect(op.get("expect").unwrap_or(&Value::from(1)))?;
                    let except: Vec<String> = op
                        .get("except")
                        .and_then(Value::as_array)
                        .map(|a| a.iter().filter_map(Value::as_str).map(str::to_owned).collect())
                        .unwrap_or_default();
                    let spec = ReplaceSpec {
                        old: text("old").unwrap_or(""),
                        new: text("new").unwrap_or(""),
                        expect,
                        table: as_index(op, "table")?,
                        except: &except,
                        dry_run: false,
                    };
                    match replace(&data, &spec)? {
                        ReplaceOutcome::Edited(out, change) => Ok((out, change)),
                        ReplaceOutcome::Preview(_) => unreachable!("not a dry run"),
                    }
                }
                "copy-row" => structure::copy_row(
                    &data,
                    req("table")?,
                    req("row")?,
                    as_index(op, "count")?.unwrap_or(1),
                    flag("clear"),
                    flag("force_locked"),
                ),
                "delete-row" => structure::delete_row(&data, req("table")?, req("row")?, flag("force_locked")),
                "copy-col" => structure::copy_col(
                    &data,
                    req("table")?,
                    req("col")?,
                    as_index(op, "count")?.unwrap_or(1),
                    flag("clear"),
                    flag("force_locked"),
                ),
                "delete-col" => structure::delete_col(&data, req("table")?, req("col")?, flag("force_locked")),
                "copy-table" => structure::copy_table(
                    &data,
                    req("table")?,
                    as_index(op, "count")?.unwrap_or(1),
                    flag("clear"),
                    flag("force_locked"),
                ),
                "delete-table" => structure::delete_table(&data, req("table")?, flag("force_locked")),
                "insert-paragraph" => {
                    let anchor = paragraph_anchor(
                        as_index(op, "after_table")?,
                        as_index(op, "before_table")?,
                        as_index(op, "after_line")?,
                    )?;
                    structure::insert_paragraph(&data, text("text").unwrap_or(""), anchor)
                }
                _ => structure::delete_paragraph(&data, req("line")?),
            }
        };
        let (next, change) = step().map_err(|e| e.context(&format!("edit #{} ({kind})", i + 1)))?;
        data = next;
        log.push(change);
    }
    Ok((data, log))
}

// ---------------------------------------------------------------------------
// writing

#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct WriteReport {
    pub output: String,
    pub bytes: u64,
    /// Issues the edit introduced that are not errors.
    pub new_warnings: Vec<Issue>,
}

fn issue_key(issue: &Issue) -> (Level, String, String) {
    // Line numbers shift with any edit above an issue; compare without them.
    let mut message = String::new();
    let mut rest = issue.message.as_str();
    while let Some(at) = rest.find("line ") {
        message.push_str(&rest[..at + 5]);
        rest = &rest[at + 5..];
        let digits = rest.bytes().take_while(u8::is_ascii_digit).count();
        if digits > 0 {
            message.push('?');
            rest = &rest[digits..];
        }
    }
    message.push_str(rest);
    (issue.level, issue.code.clone(), message)
}

/// Write `src` with member replacements unless that adds structural errors,
/// then re-read the output to confirm the replaced members.
pub fn commit(src: &Archive, replacements: &BTreeMap<String, Vec<u8>>, out: &Path, force: bool) -> Result<WriteReport> {
    guard_output(out, force, &[&src.path])?;
    let (before, _) = check_archive(src);
    let probe = src.with_replacements(replacements);
    let (mut after, _) = check_archive(&probe);
    after.extend(line_ending_issue(src.contents()?, probe.contents()?));
    let mut seen: BTreeMap<(Level, String, String), usize> = BTreeMap::new();
    for issue in &before {
        *seen.entry(issue_key(issue)).or_default() += 1;
    }
    let mut fresh = Vec::new();
    for issue in after {
        match seen.get_mut(&issue_key(&issue)) {
            Some(n) if *n > 0 => *n -= 1,
            _ => fresh.push(issue),
        }
    }
    let errors: Vec<Issue> = fresh.iter().filter(|i| i.level == Level::Error).cloned().collect();
    if !errors.is_empty() {
        return Err(Error::new(
            ErrorCode::StructuralError,
            "edit would introduce structural errors; nothing was written",
        )
        .with_issues(errors));
    }
    let bytes = archive_bytes(src, replacements)?;
    atomic_write(out, &bytes)?;
    let reread = read_archive(out)?;
    for (name, data) in replacements {
        if reread.get(name)?.data != *data {
            return Err(Error::new(
                ErrorCode::IoError,
                format!("{}: verification failed after writing {name}", out.display()),
            ));
        }
    }
    Ok(WriteReport {
        output: out.display().to_string(),
        bytes: bytes.len() as u64,
        new_warnings: fresh.into_iter().filter(|i| i.level == Level::Warning).collect(),
    })
}

pub fn commit_contents(src: &Archive, contents: Vec<u8>, out: &Path, force: bool) -> Result<WriteReport> {
    commit(src, &BTreeMap::from([(CONTENTS.to_owned(), contents)]), out, force)
}

// ---------------------------------------------------------------------------
// extract and pack

pub const MANIFEST: &str = "dsd-manifest.json";
const UTF8_BOM: &[u8] = b"\xef\xbb\xbf";

#[derive(Debug, Clone, Serialize, serde::Deserialize)]
pub struct ManifestEntry {
    pub name: String,
    pub file: String,
    pub raw_name_hex: String,
}

#[derive(Debug, Clone, Serialize, serde::Deserialize)]
pub struct Manifest {
    pub source: String,
    pub tool: String,
    pub members: Vec<ManifestEntry>,
}

fn hex(bytes: &[u8]) -> String {
    use std::fmt::Write as _;
    bytes.iter().fold(String::new(), |mut s, b| {
        let _ = write!(s, "{b:02x}");
        s
    })
}

fn safe_file_name(name: &str) -> String {
    name.chars()
        .map(|c| {
            if matches!(c, '\\' | '/' | ':' | '*' | '?' | '"' | '<' | '>' | '|') || (c as u32) < 0x20 { '_' } else { c }
        })
        .collect()
}

/// Unpack members for text editing, with a manifest that `pack` uses to
/// restore the original layout.
pub fn extract(arc: &Archive, dir: &Path, force: bool, tool: &str) -> Result<Manifest> {
    if dir.exists() && !dir.is_dir() {
        return Err(Error::new(ErrorCode::OutputExists, format!("{}: exists and is not a directory", dir.display())));
    }
    if dir.is_dir() && !force && std::fs::read_dir(dir).map_err(|e| Error::io(dir, &e))?.next().is_some() {
        return Err(Error::new(ErrorCode::OutputExists, format!("{}: directory is not empty", dir.display()))
            .with_hint("pass --force only if replacing it is authorized"));
    }
    std::fs::create_dir_all(dir).map_err(|e| Error::io(dir, &e))?;
    let mut members = Vec::new();
    for (i, m) in arc.members.iter().enumerate() {
        let rel = if m.name == CONTENTS || m.name == crate::archive::META {
            m.name.clone()
        } else {
            format!("members/{i:03}-{}", safe_file_name(&m.name))
        };
        atomic_write(&dir.join(&rel), &m.data)?;
        members.push(ManifestEntry { name: m.name.clone(), file: rel, raw_name_hex: hex(&m.raw_name) });
    }
    let manifest = Manifest { source: arc.file_name(), tool: tool.to_owned(), members };
    let mut text = serde_json::to_string_pretty(&manifest).expect("manifest serializes");
    text.push('\n');
    atomic_write(&dir.join(MANIFEST), text.as_bytes())?;
    Ok(manifest)
}

#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct PackReport {
    pub changed_members: Vec<String>,
    #[serde(flatten)]
    pub write: WriteReport,
}

/// Repack an extracted directory using the original archive's layout.
pub fn pack(dir: &Path, original: &Archive, out: &Path, force: bool) -> Result<PackReport> {
    let manifest_path = dir.join(MANIFEST);
    if !manifest_path.is_file() {
        return Err(Error::new(ErrorCode::FileNotFound, format!("{}: missing", manifest_path.display()))
            .with_hint("extract with `dsd extract` first"));
    }
    let raw = std::fs::read(&manifest_path).map_err(|e| Error::io(&manifest_path, &e))?;
    let manifest: Manifest = serde_json::from_slice(&raw)
        .map_err(|e| Error::invalid(format!("{}: not a dsd manifest ({e})", manifest_path.display())))?;
    let by_raw: BTreeMap<String, &crate::archive::Member> =
        original.members.iter().map(|m| (hex(&m.raw_name), m)).collect();
    let mut listed: Vec<&str> = manifest.members.iter().map(|e| e.raw_name_hex.as_str()).collect();
    listed.sort_unstable();
    if listed != by_raw.keys().map(String::as_str).collect::<Vec<_>>() {
        return Err(Error::invalid("the manifest does not match the original archive's members")
            .with_hint("re-extract from the original"));
    }
    let mut replacements = BTreeMap::new();
    for entry in &manifest.members {
        if entry.file.starts_with(['/', '\\'])
            || entry.file.split(['/', '\\']).any(|p| p == "..")
            || entry.file.contains(':')
        {
            return Err(Error::invalid(format!("manifest entry {} points outside the directory", quote(&entry.file))));
        }
        let path = dir.join(&entry.file);
        if !path.is_file() {
            return Err(Error::new(
                ErrorCode::FileNotFound,
                format!("{}: missing; members cannot be removed with pack", path.display()),
            ));
        }
        let data = std::fs::read(&path).map_err(|e| Error::io(&path, &e))?;
        replacements.insert(by_raw[&entry.raw_name_hex].name.clone(), data);
    }
    if replacements.get(CONTENTS).is_some_and(|c| c.starts_with(UTF8_BOM)) {
        return Err(Error::refused("contents.xml gained a UTF-8 BOM").with_hint("save it as UTF-8 without BOM"));
    }
    let changed_members = replacements
        .iter()
        .filter(|(k, v)| original.member(k).is_some_and(|m| m.data != **v))
        .map(|(k, _)| k.clone())
        .collect();
    let write = commit(original, &replacements, out, force)?;
    Ok(PackReport { changed_members, write })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn escapes_text_and_line_breaks() {
        assert_eq!(xml_escape_text("a<b>&\"c\"\r\nd"), "a&lt;b&gt;&amp;&quot;c&quot;&amp;cr;d");
    }

    #[test]
    fn sets_or_adds_attributes() {
        assert_eq!(
            set_attr_in_tag(b"<TU AUNITVALUE=\"1\" X=\"2\">", "AUNITVALUE", "3"),
            b"<TU AUNITVALUE=\"3\" X=\"2\">"
        );
        assert_eq!(set_attr_in_tag(b"<TU X=\"2\"/>", "AUNITVALUE", "a&b"), b"<TU X=\"2\" AUNITVALUE=\"a&amp;b\"/>");
    }

    #[test]
    fn decodes_runs_with_barriers() {
        let data = "a&amp;cr;&lt;<!-- x -->b\r\n가".as_bytes();
        let (chars, _) = decode_run(data, 0, data.len());
        assert_eq!(chars.into_iter().collect::<String>(), "a\n<\0b\0\0가");
    }

    #[test]
    fn issue_keys_ignore_line_numbers() {
        let a = Issue::warning("x", "line 12: thing at line 3");
        let b = Issue::warning("x", "line 7: thing at line 30");
        assert_eq!(issue_key(&a), issue_key(&b));
    }
}
