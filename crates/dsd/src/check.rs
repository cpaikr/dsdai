//! Structural checks on an archive, and comparison with an original.

use std::collections::{BTreeMap, BTreeSet};

use serde::Serialize;

use crate::archive::{Archive, CONTENTS, META, NameEncoding};
use crate::doc::{Cell, Doc, OBSERVED_SCHEMAS, Table, attr_int};
use crate::error::Result;
use crate::text::{header_info, node_text, quote, unverified_nodes, unverified_schema};
use crate::xml::{self, Tree};

#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize)]
#[serde(rename_all = "lowercase")]
pub enum Level {
    Error,
    Warning,
    Info,
}

impl Level {
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::Error => "error",
            Self::Warning => "warning",
            Self::Info => "info",
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct Issue {
    pub level: Level,
    pub code: String,
    pub message: String,
}

impl Issue {
    pub fn new(level: Level, code: &str, message: impl Into<String>) -> Self {
        Self { level, code: code.to_owned(), message: message.into() }
    }

    pub fn error(code: &str, message: impl Into<String>) -> Self {
        Self::new(Level::Error, code, message)
    }

    pub fn warning(code: &str, message: impl Into<String>) -> Self {
        Self::new(Level::Warning, code, message)
    }

    pub fn info(code: &str, message: impl Into<String>) -> Self {
        Self::new(Level::Info, code, message)
    }
}

pub fn has_errors(issues: &[Issue]) -> bool {
    issues.iter().any(|i| i.level == Level::Error)
}

/// Parse `meta.xml`, refusing declarations.
pub fn parse_meta(data: &[u8]) -> Result<Tree> {
    if data.windows(9).any(|w| w == b"<!DOCTYPE") || data.windows(8).any(|w| w == b"<!ENTITY") {
        return Err(crate::Error::new(crate::ErrorCode::InvalidXml, "meta.xml contains DOCTYPE/entity declarations"));
    }
    xml::parse(data).map_err(|e| {
        crate::Error::new(
            crate::ErrorCode::InvalidXml,
            format!("meta.xml is not well-formed: {}", strip_prefix(&e.message)),
        )
    })
}

fn strip_prefix(message: &str) -> &str {
    message.strip_prefix("XML is not well-formed: ").unwrap_or(message)
}

/// The `meta.xml` GENERATOR attribute, when present.
pub fn meta_generator(meta: &Tree, name: &str) -> Option<String> {
    meta.child_tag(meta.root, "GENERATOR").and_then(|g| meta.node(g).attr(name)).map(str::to_owned)
}

pub fn img_refs(doc: &Doc) -> Vec<String> {
    doc.tree.iter_tag(doc.root(), "IMG").into_iter().map(|n| node_text(&doc.tree, n, false)).collect()
}

fn count(data: &[u8], needle: &[u8]) -> usize {
    data.windows(needle.len()).filter(|w| *w == needle).count()
}

pub fn line_ending_counts(data: &[u8]) -> (usize, usize) {
    (count(data, b"\r\n"), data.iter().filter(|&&b| b == b'\n').count())
}

fn py_list<T: std::fmt::Display>(items: &[T]) -> String {
    format!("[{}]", items.iter().map(ToString::to_string).collect::<Vec<_>>().join(", "))
}

/// Structural checks: members, encoding, XML, images, TOC ids, table grids,
/// size conventions, unit values, and repeated period copies.
pub fn check_archive(arc: &Archive) -> (Vec<Issue>, Option<Doc>) {
    let mut issues = Vec::new();
    for required in [CONTENTS, META] {
        if arc.member(required).is_none() {
            issues.push(Issue::error("missing-member", format!("{required} is missing")));
        }
    }
    if has_errors(&issues) {
        return (issues, None);
    }
    for m in &arc.members {
        if m.name_encoding == NameEncoding::Cp437 {
            issues.push(Issue::warning(
                "name-encoding",
                format!("member name not decodable as CP949: {}", quote(&m.name)),
            ));
        }
    }
    let contents = &arc.member(CONTENTS).expect("checked above").data;
    if let Err(e) = std::str::from_utf8(contents) {
        issues.push(Issue::error(
            "contents-encoding",
            format!("contents.xml is not valid UTF-8: invalid byte at position {}", e.valid_up_to()),
        ));
        return (issues, None);
    }
    let doc = match Doc::parse(contents.clone()) {
        Ok(doc) => doc,
        Err(e) => {
            issues.push(Issue::error("contents-xml", format!("contents.xml: {}", e.message)));
            return (issues, None);
        }
    };
    let tree = &doc.tree;
    if tree.tag(doc.root()) != "DOCUMENT" {
        issues.push(Issue::error(
            "root",
            format!("contents.xml root is <{}>, expected <DOCUMENT>", tree.tag(doc.root())),
        ));
    }
    let meta = match parse_meta(&arc.member(META).expect("checked above").data) {
        Ok(m) => Some(m),
        Err(e) => {
            issues.push(Issue::error("meta-xml", e.message));
            None
        }
    };
    let head = header_info(&doc);
    if let Some(schema) = unverified_schema(&doc) {
        issues.push(Issue::warning(
            "unverified-schema",
            format!(
                "schema {schema} is not one this tool was verified on ({}); edit commands refuse this file",
                OBSERVED_SCHEMAS.join(", ")
            ),
        ));
    }
    let mut unseen: BTreeMap<String, Vec<usize>> = BTreeMap::new();
    for node in unverified_nodes(tree, doc.root()) {
        unseen.entry(tree.tag(node).to_owned()).or_default().push(node);
    }
    for (tag, nodes) in &unseen {
        issues.push(Issue::warning(
            "unverified-element",
            format!(
                "<{tag}> x{}, first at line {}: not seen in verified DART output; readers mark it and edit commands refuse to touch it",
                nodes.len(),
                doc.line(tree.node(nodes[0]).start)
            ),
        ));
    }
    let mut meta_imgs: Vec<String> = Vec::new();
    if let Some(meta) = &meta {
        let meta_schema = meta_generator(meta, "schema").unwrap_or_default();
        if !meta_schema.is_empty()
            && !head.schema.is_empty()
            && meta_schema.to_lowercase() != head.schema.to_lowercase()
        {
            issues.push(Issue::warning(
                "schema-mismatch",
                format!("meta.xml schema {meta_schema} != contents.xml {}", head.schema),
            ));
        }
        meta_imgs = meta
            .iter_tag(meta.root, "IMG")
            .into_iter()
            .map(|n| meta.node(n).attr("filename").unwrap_or("").to_owned())
            .collect();
    }
    let member_imgs: BTreeSet<String> = arc.images().map(|m| m.name.clone()).collect();
    let refs: BTreeSet<String> = img_refs(&doc).into_iter().collect();
    let meta_set: BTreeSet<String> = meta_imgs.into_iter().collect();
    for r in refs.difference(&member_imgs) {
        issues
            .push(Issue::error("image-missing", format!("contents.xml references image not in archive: {}", quote(r))));
    }
    if meta.is_some() {
        for r in meta_set.difference(&member_imgs) {
            issues.push(Issue::error("image-missing", format!("meta.xml lists image not in archive: {}", quote(r))));
        }
        for name in member_imgs.difference(&meta_set) {
            issues.push(Issue::warning(
                "image-unlisted",
                format!("archive image missing from meta.xml list: {}", quote(name)),
            ));
        }
    }
    for name in member_imgs.difference(&refs) {
        issues.push(Issue::warning(
            "image-unreferenced",
            format!("archive image not referenced in contents.xml: {}", quote(name)),
        ));
    }

    let (crlf, lf) = line_ending_counts(&doc.data);
    if crlf > 0 && lf != crlf {
        issues.push(Issue::warning(
            "line-endings",
            format!("mixed line endings ({} bare LF among {crlf} CRLF)", lf - crlf),
        ));
    }

    let mut toc_ids: Vec<i64> = Vec::new();
    for node in tree.descendants(doc.root()) {
        let n = tree.node(node);
        if n.tag != "BODY"
            && let Some(v) = n.attr("ATOCID")
        {
            match attr_int(v) {
                Some(id) => toc_ids.push(id),
                None => {
                    issues.push(Issue::warning("toc-id", format!("line {}: non-numeric ATOCID", doc.line(n.start))));
                }
            }
        }
    }
    let mut seen = BTreeMap::new();
    for id in &toc_ids {
        *seen.entry(*id).or_insert(0usize) += 1;
    }
    let dupes: Vec<i64> = seen.iter().filter(|(_, c)| **c > 1).map(|(id, _)| *id).collect();
    if !dupes.is_empty() {
        issues.push(Issue::warning(
            "toc-id",
            format!("duplicate ATOCID values: {}", py_list(&dupes[..dupes.len().min(10)])),
        ));
    }
    if let Some(body) = tree.first_tag(doc.root(), "BODY")
        && let Some(max) = toc_ids.iter().max()
        && let Some(atoc) =
            tree.node(body).attr("ATOCID").filter(|v| !v.is_empty() && v.bytes().all(|b| b.is_ascii_digit()))
        && atoc.parse::<i64>().ok() != Some(*max)
    {
        issues.push(Issue::warning("toc-id", format!("BODY ATOCID={atoc} but highest descendant ATOCID is {max}")));
    }

    for table in &doc.tables {
        let place = format!("T{} (line {})", table.index, doc.line(tree.node(table.node).start));
        for problem in &table.issues {
            issues.push(Issue::error("table-grid", format!("{place} {problem}")));
        }
        issues.extend(width_issues(&doc, table, &place));
        for cell in &table.cells {
            issues.extend(unit_issue(&doc, table, cell));
        }
    }
    issues.extend(period_issues(&doc));
    (issues, Some(doc))
}

fn int_attr_strict(doc: &Doc, node: usize, name: &str) -> Option<i64> {
    doc.attr(node, name).and_then(attr_int)
}

/// Compare cell WIDTH/HEIGHT with the table's own sizing convention.
///
/// Observed: Dart4-era editors write cell WIDTH = sum(spanned COL) - 9; an
/// older Dart3-era editor wrote cell WIDTH = sum(spanned COL) + 9 x COLSPAN.
/// Cell HEIGHT = sum(spanned TR HEIGHT) - 7 in both.
fn width_issues(doc: &Doc, table: &Table, place: &str) -> Vec<Issue> {
    if table.cols.iter().all(|&c| c == 0) || table.cells.is_empty() {
        return Vec::new();
    }
    let (mut modern_hits, mut legacy_hits) = (0, 0);
    let mut measured: Vec<(&Cell, i64, i64)> = Vec::new();
    for cell in &table.cells {
        let Some(width) = int_attr_strict(doc, cell.node, "WIDTH") else { continue };
        let end = (cell.col - 1 + cell.colspan).min(table.cols.len());
        let span: i64 = table.cols[(cell.col - 1).min(end)..end].iter().sum();
        let (modern, legacy) = (span - 9, span + 9 * i64::try_from(cell.colspan).unwrap_or(i64::MAX / 18));
        measured.push((cell, width - modern, width - legacy));
        modern_hits += usize::from(width == modern);
        legacy_hits += usize::from(width == legacy);
    }
    let modern = modern_hits >= legacy_hits;
    let mut out = Vec::new();
    let off: Vec<&Cell> =
        measured.iter().filter(|(_, dm, dl)| if modern { *dm != 0 } else { *dl != 0 }).map(|(c, _, _)| *c).collect();
    if !off.is_empty() && off.len() < measured.len() {
        let sample = off.iter().take(5).map(|c| format!("R{}C{}", c.row, c.col)).collect::<Vec<_>>().join(", ");
        out.push(Issue::warning(
            "cell-width",
            format!("{place}: {} cell WIDTH value(s) differ from the table's convention ({sample})", off.len()),
        ));
    }
    let mut heights = Vec::new();
    for cell in &table.cells {
        let Some(h) = int_attr_strict(doc, cell.node, "HEIGHT") else { continue };
        let rows = &table.rows[cell.row - 1..(cell.row - 1 + cell.rowspan).min(table.rows.len())];
        let Some(sum) = rows.iter().map(|&r| int_attr_strict(doc, r, "HEIGHT")).sum::<Option<i64>>() else { continue };
        if h != sum - 7 {
            heights.push(format!("R{}C{}", cell.row, cell.col));
        }
    }
    if !heights.is_empty() {
        out.push(Issue::warning(
            "cell-height",
            format!(
                "{place}: cell HEIGHT differs from row HEIGHT - 7 at {}",
                heights[..heights.len().min(5)].join(", ")
            ),
        ));
    }
    out
}

/// Python `\d` for the digits documents use: ASCII and full-width.
fn digit_value(c: char) -> Option<u32> {
    match c {
        '0'..='9' => Some(c as u32 - '0' as u32),
        '\u{FF10}'..='\u{FF19}' => Some(c as u32 - 0xFF10),
        _ => None,
    }
}

fn digit_runs(text: &str) -> Vec<String> {
    let mut runs = Vec::new();
    let mut cur = String::new();
    for c in text.chars() {
        if let Some(d) = digit_value(c) {
            cur.push(char::from_digit(d, 10).expect("single digit"));
        } else if !cur.is_empty() {
            runs.push(std::mem::take(&mut cur));
        }
    }
    if !cur.is_empty() {
        runs.push(cur);
    }
    runs
}

/// `YYYYMMDD` from the first three digit runs when the first is a year.
pub fn date_digits(text: &str) -> String {
    let runs = digit_runs(text);
    if runs.len() >= 3 && runs[0].chars().count() == 4 {
        let n = |s: &str| s.trim_start_matches('0').parse::<u64>().unwrap_or(0);
        return format!("{:04}{:02}{:02}", n(&runs[0]), n(&runs[1]), n(&runs[2]));
    }
    String::new()
}

fn is_date_unit(aunit: &str) -> bool {
    ["DT", "DATE", "PERIODFROM", "PERIODTO"].iter().any(|k| aunit.contains(k))
}

fn is_eight_digits(value: &str) -> bool {
    value.chars().count() == 8 && value.chars().all(|c| digit_value(c).is_some())
}

/// Observed: the currency-unit TU (AUNIT=WON) stores a code, not the
/// multiplier, so "1000" for 천원 is wrong even though it looks plausible.
pub const WON_CODES: [(&str, &str); 3] = [("1", "원"), ("2", "천원"), ("3", "백만원")];

fn is_hangul_syllable(c: char) -> bool {
    ('\u{AC00}'..='\u{D7A3}').contains(&c)
}

/// The first whole currency-unit token: 원 inside a word (직원, 원화) is not a unit.
fn won_token(chars: &[char]) -> Option<String> {
    for i in 0..chars.len() {
        if i > 0 && is_hangul_syllable(chars[i - 1]) {
            continue;
        }
        for prefix in ["백만", "천", "만", ""] {
            let p: Vec<char> = prefix.chars().chain(std::iter::once('원')).collect();
            if chars[i..].starts_with(&p) && chars.get(i + p.len()).is_none_or(|c| !is_hangul_syllable(*c)) {
                return Some(p.into_iter().collect());
            }
        }
    }
    None
}

/// The currency unit the text names, preferring the one after a 단위 label.
pub fn won_text_unit(text: &str) -> String {
    // Match on the original spacing: removing it would glue words together.
    let chars: Vec<char> = text.chars().collect();
    let label = chars.windows(2).position(|w| w == ['단', '위']);
    if let Some(at) = label
        && let Some(t) = won_token(&chars[at..])
    {
        return t;
    }
    won_token(&chars).unwrap_or_default()
}

/// Why `value` cannot be the AUNITVALUE of a TU cell showing `text`, or None.
///
/// Only verified unit kinds are judged: dates (YYYYMMDD) and WON codes. Text
/// the rules cannot read is not compared, but an unobserved code for a coded
/// kind is always reported.
pub fn unit_value_conflict(aunit: &str, text: &str, value: &str) -> Option<String> {
    if value.is_empty() {
        return None;
    }
    if is_date_unit(aunit) && is_eight_digits(value) {
        let shown = date_digits(text);
        return (!shown.is_empty() && shown != value)
            .then(|| format!("text shows date {shown} but AUNITVALUE is {value}"));
    }
    if aunit != "WON" {
        return None;
    }
    let known = WON_CODES.iter().map(|(c, n)| format!("{c}={n}")).collect::<Vec<_>>().join(", ");
    let Some((_, expected)) = WON_CODES.iter().find(|(c, _)| *c == value) else {
        return Some(format!("AUNIT={aunit} takes a code ({known}), not {}", quote(value)));
    };
    let shown = won_text_unit(text);
    if let Some((code, _)) = WON_CODES.iter().find(|(_, n)| *n == shown)
        && shown != *expected
    {
        return Some(format!("text shows {shown} (code {code}) but AUNITVALUE is {value} ({expected})"));
    }
    None
}

fn unit_issue(doc: &Doc, table: &Table, cell: &Cell) -> Option<Issue> {
    if doc.tag(cell.node) != "TU" {
        return None;
    }
    let unit = doc.attr(cell.node, "AUNIT").unwrap_or("");
    let text = node_text(&doc.tree, cell.node, false);
    let conflict = unit_value_conflict(unit, &text, doc.attr(cell.node, "AUNITVALUE").unwrap_or(""))?;
    Some(Issue::warning("unit-value", format!("T{}R{}C{}: TU {unit}: {conflict}", table.index, cell.row, cell.col)))
}

/// Warn when TU cells for the same reporting-period bound disagree.
///
/// Observed: forms that repeat the period write the repeat as
/// PERIODFROM2/PERIODTO2 with the same machine date, so a lone edit to one
/// copy leaves the other stale.
fn period_issues(doc: &Doc) -> Vec<Issue> {
    let mut bounds: BTreeMap<&str, Vec<(String, String)>> = BTreeMap::new();
    for table in &doc.tables {
        for cell in &table.cells {
            let unit = doc.attr(cell.node, "AUNIT").unwrap_or("");
            let bound = ["PERIODFROM", "PERIODTO"].into_iter().find(|b| unit == *b || unit == format!("{b}2"));
            if doc.tag(cell.node) == "TU"
                && let Some(bound) = bound
                && let Some(value) = doc.attr(cell.node, "AUNITVALUE").filter(|v| !v.is_empty())
            {
                bounds
                    .entry(bound)
                    .or_default()
                    .push((format!("T{}R{}C{}", table.index, cell.row, cell.col), value.to_owned()));
            }
        }
    }
    bounds
        .into_iter()
        .filter(|(_, cells)| cells.iter().map(|(_, v)| v).collect::<BTreeSet<_>>().len() > 1)
        .map(|(bound, cells)| {
            let listing = cells.iter().map(|(loc, v)| format!("{loc}={v}")).collect::<Vec<_>>().join(", ");
            Issue::warning("period-mismatch", format!("{bound}/{bound}2 TU cells disagree: {listing}"))
        })
        .collect()
}

pub fn compare_members(orig: &Archive, new: &Archive) -> Vec<Issue> {
    let mut out = Vec::new();
    let layout = |a: &Archive| a.members.iter().map(|m| (m.raw_name.clone(), m.utf8_flag())).collect::<Vec<_>>();
    if layout(orig) != layout(new) {
        out.push(Issue::warning("members-changed", "member names, order, or name flags differ from the original"));
    }
    for om in &orig.members {
        if let Some(nm) = new.members.iter().find(|m| m.raw_name == om.raw_name)
            && om.name != CONTENTS
            && nm.data != om.data
        {
            out.push(Issue::warning("member-data", format!("{} differs from the original", om.name)));
        }
    }
    out
}

/// An error when an all-CRLF `contents.xml` gained bare LF line endings.
pub fn line_ending_issue(old: &[u8], new: &[u8]) -> Option<Issue> {
    let (crlf_old, lf_old) = line_ending_counts(old);
    let (crlf_new, lf_new) = line_ending_counts(new);
    (crlf_old > 0 && crlf_old == lf_old && crlf_new != lf_new).then(|| {
        Issue::error(
            "line-endings",
            format!(
                "contents.xml gained {} bare LF line ending(s); the original is CRLF throughout",
                lf_new - crlf_new
            ),
        )
    })
}

/// Differences between an original and an edited archive, for `check --against`.
pub fn compare_archives(orig: &Archive, new: &Archive) -> Result<Vec<Issue>> {
    let mut out = compare_members(orig, new);
    out.extend(line_ending_issue(orig.contents()?, new.contents()?));
    let odoc = Doc::parse(orig.contents()?.to_vec())?;
    let ndoc = Doc::parse(new.contents()?.to_vec())?;
    for tag in ["TABLE", "SECTION-1", "SECTION-2", "TITLE", "IMAGE", "INSERTION", "LIBRARY"] {
        let (a, b) = (odoc.tree.iter_tag(odoc.root(), tag).len(), ndoc.tree.iter_tag(ndoc.root(), tag).len());
        if a != b {
            out.push(Issue::info("structure", format!("{tag} count {a} -> {b}")));
        }
    }
    let rows = |d: &Doc| d.tables.iter().map(|t| t.rows.len()).sum::<usize>();
    if rows(&odoc) != rows(&ndoc) {
        out.push(Issue::info("structure", format!("table rows {} -> {}", rows(&odoc), rows(&ndoc))));
    }
    let changed = crate::diff::block_diff(&odoc, &ndoc).len();
    let raw = crate::diff::raw_changed_line_count(orig.contents()?, new.contents()?);
    out.push(Issue::info(
        "changes",
        format!("{changed} changed cell/text block(s), {raw} changed raw line(s); review with `dsd diff`"),
    ));
    Ok(out)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn won_units_match_whole_tokens() {
        assert_eq!(won_text_unit("(단위: 천원)"), "천원");
        assert_eq!(won_text_unit("직원 수"), "");
        assert_eq!(won_text_unit("원화 금액 (단위 : 백만원)"), "백만원");
        assert_eq!(won_text_unit("금액천원"), "");
        assert_eq!(won_text_unit("원"), "원");
    }

    #[test]
    fn unit_conflicts() {
        assert!(unit_value_conflict("WON", "(단위: 천원)", "1000").is_some());
        assert!(unit_value_conflict("WON", "(단위: 천원)", "2").is_none());
        assert!(unit_value_conflict("WON", "(단위: 천원)", "3").is_some());
        assert!(unit_value_conflict("PERIODTO", "2025년 12월 31일", "20251231").is_none());
        assert!(unit_value_conflict("PERIODTO", "2025년 12월 31일", "20241231").is_some());
        assert_eq!(date_digits("２０２５년 1월 2일"), "20250102");
    }
}
