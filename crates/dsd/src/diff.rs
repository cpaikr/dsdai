//! Block-level and raw-line differences between two documents.

use crate::difflib::{Matcher, Tag};
use crate::doc::Doc;
use crate::text::{Block, blocks};

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ChangeKind {
    Changed,
    Removed,
    Added,
}

#[derive(Debug, Clone)]
pub struct Change {
    pub kind: ChangeKind,
    pub old: Option<Block>,
    pub new: Option<Block>,
}

fn change(old: Option<&Block>, new: Option<&Block>) -> Change {
    let kind = match (old, new) {
        (Some(_), Some(_)) => ChangeKind::Changed,
        (Some(_), None) => ChangeKind::Removed,
        _ => ChangeKind::Added,
    };
    Change { kind, old: old.cloned(), new: new.cloned() }
}

/// Blocks grouped into diff units: one per table row, one per text block.
fn units(doc: &Doc) -> Vec<Vec<Block>> {
    let mut out: Vec<Vec<Block>> = Vec::new();
    for block in blocks(doc) {
        match out.last_mut() {
            Some(unit) if unit[0].group == block.group => unit.push(block),
            _ => out.push(vec![block]),
        }
    }
    out
}

fn pair_blocks(olds: &[Block], news: &[Block], out: &mut Vec<Change>) {
    if olds.len() == news.len() {
        // Same shape: an in-place edit, so pair by position.
        for (o, n) in olds.iter().zip(news) {
            if o.key() != n.key() {
                out.push(change(Some(o), Some(n)));
            }
        }
        return;
    }
    let ok: Vec<_> = olds.iter().map(Block::key).collect();
    let nk: Vec<_> = news.iter().map(Block::key).collect();
    for (tag, i1, i2, j1, j2) in Matcher::new(&ok, &nk).opcodes() {
        if tag == Tag::Equal {
            continue;
        }
        let (a, b) = (&olds[i1..i2], &news[j1..j2]);
        for k in 0..a.len().max(b.len()) {
            out.push(change(a.get(k), b.get(k)));
        }
    }
}

/// Units of one kind may pair: rows of the same table, or text blocks with the same tag.
fn unit_kind(unit: &[Block]) -> String {
    let group = &unit[0].group;
    if group.starts_with('T') {
        group.split('R').next().unwrap_or(group).to_owned()
    } else {
        format!("<{}>", unit[0].tag)
    }
}

/// Cells equal at the same position, plus a 0..1 text resemblance tie-breaker.
fn unit_similarity(o: &[Block], n: &[Block]) -> f64 {
    let same = o.iter().zip(n).filter(|(a, b)| a.key() == b.key()).count();
    let join =
        |u: &[Block]| u.iter().map(|b| b.text.as_str()).collect::<Vec<_>>().join("\u{1f}").chars().collect::<Vec<_>>();
    let (a, b) = (join(o), join(n));
    same as f64 + Matcher::new(&a, &b).ratio()
}

/// Above this many candidate unit pairs in one changed region, pairing falls
/// back to position; such regions are wholesale rewrites, not row edits.
const MAX_PAIRING_WORK: usize = 40_000;

/// Diff a changed region whose row counts differ.
///
/// Each old unit pairs with at most one new unit, in order, maximizing total
/// similarity; only paired units are compared cell by cell, and unpaired ones
/// are reported whole. A cleared copy-row keeps its template's attributes, so
/// attributes alone cannot tell an inserted row from an edited neighbour.
#[allow(clippy::float_cmp)]
fn align_units(olds: &[Vec<Block>], news: &[Vec<Block>], out: &mut Vec<Change>) {
    let (m, n) = (olds.len(), news.len());
    if m * n > MAX_PAIRING_WORK {
        for k in 0..m.max(n) {
            pair_blocks(olds.get(k).map_or(&[], Vec::as_slice), news.get(k).map_or(&[], Vec::as_slice), out);
        }
        return;
    }
    let kinds_o: Vec<String> = olds.iter().map(|u| unit_kind(u)).collect();
    let kinds_n: Vec<String> = news.iter().map(|u| unit_kind(u)).collect();
    let mut sim = vec![vec![0.0f64; n]; m];
    for i in 0..m {
        for j in 0..n {
            if kinds_o[i] == kinds_n[j] {
                sim[i][j] = unit_similarity(&olds[i], &news[j]);
            }
        }
    }
    let mut score = vec![vec![0.0f64; n + 1]; m + 1];
    for i in (0..m).rev() {
        for j in (0..n).rev() {
            let mut best = score[i + 1][j].max(score[i][j + 1]);
            if kinds_o[i] == kinds_n[j] {
                best = best.max(score[i + 1][j + 1] + sim[i][j]);
            }
            score[i][j] = best;
        }
    }
    let (mut i, mut j) = (0, 0);
    // Exact equality retraces the choices the score table recorded.
    while i < m || j < n {
        if i < m && j < n && kinds_o[i] == kinds_n[j] && score[i][j] == score[i + 1][j + 1] + sim[i][j] {
            pair_blocks(&olds[i], &news[j], out);
            i += 1;
            j += 1;
        } else if i < m && (j == n || score[i][j] == score[i + 1][j]) {
            pair_blocks(&olds[i], &[], out);
            i += 1;
        } else {
            pair_blocks(&[], &news[j], out);
            j += 1;
        }
    }
}

/// Changed blocks, aligned row by row before cell by cell.
///
/// Matching individual cells lets an empty cell at the end of one row pair
/// with an identical empty cell at the end of an inserted row, which reports
/// the insertion one cell early. Aligning whole rows first keeps an inserted
/// or deleted row in one piece at its real coordinates.
pub fn block_diff(odoc: &Doc, ndoc: &Doc) -> Vec<Change> {
    let (ou, nu) = (units(odoc), units(ndoc));
    let key = |u: &Vec<Block>| u.iter().map(|b| (b.tag.clone(), b.text.clone(), b.attrs.clone())).collect::<Vec<_>>();
    let ok: Vec<_> = ou.iter().map(key).collect();
    let nk: Vec<_> = nu.iter().map(key).collect();
    let mut out = Vec::new();
    for (tag, i1, i2, j1, j2) in Matcher::new(&ok, &nk).opcodes() {
        match tag {
            Tag::Equal => {}
            Tag::Replace if i2 - i1 == j2 - j1 => {
                for (o, n) in ou[i1..i2].iter().zip(&nu[j1..j2]) {
                    pair_blocks(o, n, &mut out);
                }
            }
            _ => align_units(&ou[i1..i2], &nu[j1..j2], &mut out),
        }
    }
    out
}

/// Lines split like Python's `bytes.splitlines()` (LF, CRLF, or CR).
pub fn split_lines(data: &[u8]) -> Vec<&[u8]> {
    let mut out = Vec::new();
    let mut start = 0;
    let mut i = 0;
    while i < data.len() {
        match data[i] {
            b'\n' => {
                out.push(&data[start..i]);
                start = i + 1;
            }
            b'\r' => {
                out.push(&data[start..i]);
                if data.get(i + 1) == Some(&b'\n') {
                    i += 1;
                }
                start = i + 1;
            }
            _ => {}
        }
        i += 1;
    }
    if start < data.len() {
        out.push(&data[start..]);
    }
    out
}

/// A changed line range: 1-based, half-open `(old_start, old_end, new_start, new_end)`.
pub type LineRange = (usize, usize, usize, usize);

pub fn raw_line_changes(old: &[u8], new: &[u8]) -> Vec<LineRange> {
    let (a, b) = (split_lines(old), split_lines(new));
    Matcher::new(&a, &b)
        .opcodes()
        .into_iter()
        .filter(|op| op.0 != Tag::Equal)
        .map(|(_, i1, i2, j1, j2)| (i1 + 1, i2 + 1, j1 + 1, j2 + 1))
        .collect()
}

pub fn changed_line_count(ranges: &[LineRange]) -> usize {
    ranges.iter().map(|&(i1, i2, j1, j2)| (i2 - i1).max(j2 - j1)).sum()
}

pub fn raw_changed_line_count(old: &[u8], new: &[u8]) -> usize {
    changed_line_count(&raw_line_changes(old, new))
}
