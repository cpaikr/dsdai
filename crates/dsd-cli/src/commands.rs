//! Command handlers: adapt library results to the process contract.

use std::collections::BTreeMap;
use std::fmt::Write as _;
use std::path::Path;

use serde_json::{Value, json};
use sha2::{Digest, Sha256};

use dsd::archive::{CONTENTS, atomic_write, guard_output, same_file};
use dsd::check::{Level, check_archive, compare_archives, compare_members, has_errors};
use dsd::diff::{ChangeKind, block_diff, changed_line_count, raw_line_changes, split_lines};
use dsd::edit::{self, Change, ReplaceOutcome, ReplaceSpec, WriteReport};
use dsd::structure;
use dsd::text::{Block, quote};
use dsd::{Archive, Error, ErrorCode, Issue, document, read_archive, views};

use crate::cli::{Command, CompareFormat, Format};
use crate::envelope::{Failure, Warning};
use crate::{release_check, upgrade};

pub enum Output {
    Json { result: Value, warnings: Vec<Warning> },
    Text(String),
}

type Outcome = Result<Output, Failure>;

#[allow(clippy::unnecessary_wraps)] // Shapes every handler's success the same way.
fn json_ok(result: impl serde::Serialize) -> Outcome {
    Ok(Output::Json { result: serde_json::to_value(result).expect("results serialize"), warnings: Vec::new() })
}

fn tool() -> String {
    format!("dsd {}", crate::VERSION)
}

pub fn run(command: Command) -> Outcome {
    match command {
        Command::Inspect { file } => json_ok(views::inspect(&read_archive(&file)?)?),
        Command::Outline { file, all, format } => {
            let entries = views::outline(&document(&read_archive(&file)?)?, all);
            match format.format {
                Format::Text => Ok(Output::Text(views::render_outline(&entries))),
                Format::Json => json_ok(json!({ "entries": entries })),
            }
        }
        Command::Read { file, output, force, scaffold, no_header, format } => {
            read(&file, output.as_deref(), force, scaffold, no_header, format.format)
        }
        Command::Table { file, tables, spans, format } => table(&file, &tables, spans, format.format),
        Command::Find { file, text, exact, limit, format } => {
            let found = views::find(&document(&read_archive(&file)?)?, &text, exact, limit as usize)?;
            match format.format {
                Format::Text => Ok(Output::Text(views::render_find(&found))),
                Format::Json => json_ok(found),
            }
        }
        Command::Check { file, against, schema_dir, no_schema } => {
            check(&file, against.as_deref(), schema_dir.as_deref(), no_schema)
        }
        Command::Compare { base, other, format, output, force, all, limit } => {
            compare(&base, &other, format, output.as_deref(), force, all, limit)
        }
        Command::Diff { original, edited, limit, raw, format } => diff(&original, &edited, limit, raw, format.format),
        Command::SetCell { file, write, target, col, text, unit_value, flatten } => {
            let src = read_archive(&file)?;
            let (data, change) =
                edit::set_cell(src.contents()?, target.table, target.row, col, &text, unit_value.as_deref(), flatten)?;
            finish(&src, data, &[change], &write.output, write.force)
        }
        Command::Replace { file, output, force, dry_run, old, new, expect, table, except } => {
            let src = read_archive(&file)?;
            let expect = edit::parse_expect(&Value::String(expect))?;
            let spec = ReplaceSpec { old: &old, new: &new, expect, table, except: &except, dry_run };
            match edit::replace(src.contents()?, &spec)? {
                ReplaceOutcome::Preview(preview) => json_ok(preview),
                ReplaceOutcome::Edited(data, change) => {
                    let out = output.expect("clap requires --output without --dry-run");
                    finish(&src, data, &[change], &out, force)
                }
            }
        }
        Command::CopyRow { file, write, target, count, clear, force_locked } => {
            let src = read_archive(&file)?;
            let (data, change) =
                structure::copy_row(src.contents()?, target.table, target.row, count as usize, clear, force_locked)?;
            finish(&src, data, &[change], &write.output, write.force)
        }
        Command::DeleteRow { file, write, target, force_locked } => {
            let src = read_archive(&file)?;
            let (data, change) = structure::delete_row(src.contents()?, target.table, target.row, force_locked)?;
            finish(&src, data, &[change], &write.output, write.force)
        }
        Command::CopyCol { file, write, target, count, clear, force_locked } => {
            let src = read_archive(&file)?;
            let (data, change) =
                structure::copy_col(src.contents()?, target.table, target.col, count as usize, clear, force_locked)?;
            finish(&src, data, &[change], &write.output, write.force)
        }
        Command::DeleteCol { file, write, target, force_locked } => {
            let src = read_archive(&file)?;
            let (data, change) = structure::delete_col(src.contents()?, target.table, target.col, force_locked)?;
            finish(&src, data, &[change], &write.output, write.force)
        }
        Command::CopyTable { file, write, table, count, clear, force_locked } => {
            let src = read_archive(&file)?;
            let (data, change) = structure::copy_table(src.contents()?, table, count as usize, clear, force_locked)?;
            finish(&src, data, &[change], &write.output, write.force)
        }
        Command::DeleteTable { file, write, table, force_locked } => {
            let src = read_archive(&file)?;
            let (data, change) = structure::delete_table(src.contents()?, table, force_locked)?;
            finish(&src, data, &[change], &write.output, write.force)
        }
        Command::InsertParagraph { file, write, text, after_table, before_table, after_line } => {
            let src = read_archive(&file)?;
            let anchor = edit::paragraph_anchor(after_table, before_table, after_line)?;
            let (data, change) = structure::insert_paragraph(src.contents()?, &text, anchor)?;
            finish(&src, data, &[change], &write.output, write.force)
        }
        Command::DeleteParagraph { file, write, line } => {
            let src = read_archive(&file)?;
            let (data, change) = structure::delete_paragraph(src.contents()?, line)?;
            finish(&src, data, &[change], &write.output, write.force)
        }
        Command::Apply { file, edits, write } => {
            let src = read_archive(&file)?;
            let ops = read_edits(&edits)?;
            let (data, changes) = edit::apply(src.contents()?, &ops)?;
            finish(&src, data, &changes, &write.output, write.force)
        }
        Command::Extract { file, directory, force } => {
            let arc = read_archive(&file)?;
            let manifest = edit::extract(&arc, &directory, force, &tool())?;
            json_ok(json!({
                "directory": directory.display().to_string(),
                "contents": directory.join(CONTENTS).display().to_string(),
                "manifest": directory.join(edit::MANIFEST).display().to_string(),
                "members": manifest.members,
            }))
        }
        Command::Pack { directory, original, write } => {
            let src = read_archive(&original)?;
            let report = edit::pack(&directory, &src, &write.output, write.force)?;
            let warnings = report.write.new_warnings.iter().cloned().map(Warning::from).collect();
            let mut result = write_result(&report.write)?;
            result["changedMembers"] = json!(report.changed_members);
            Ok(Output::Json { result, warnings })
        }
        Command::Version { check } => {
            let mut result = json!({
                "name": "dsd",
                "version": crate::VERSION,
                "target": release_check::TARGET,
            });
            if check {
                let report = release_check::explicit_check();
                result["nextStep"] = json!(report.update_help());
                result["releaseCheck"] = json!(report);
            }
            json_ok(result)
        }
        Command::Upgrade { check } => Ok(Output::Json { result: upgrade::run(check)?, warnings: Vec::new() }),
    }
}

fn sha256_hex(data: &[u8]) -> String {
    Sha256::digest(data).iter().fold(String::new(), |mut s, b| {
        let _ = write!(s, "{b:02x}");
        s
    })
}

fn write_result(report: &WriteReport) -> Result<Value, Failure> {
    let path = Path::new(&report.output);
    let bytes = std::fs::read(path).map_err(|e| Error::io(path, &e))?;
    Ok(json!({ "output": report.output, "bytes": report.bytes, "sha256": sha256_hex(&bytes) }))
}

fn finish(src: &Archive, data: Vec<u8>, changes: &[Change], out: &Path, force: bool) -> Outcome {
    let report = edit::commit_contents(src, data, out, force)?;
    let mut warnings: Vec<Warning> =
        changes.iter().flat_map(|c| c.notes.iter().map(|n| Warning::new("note", n.clone()))).collect();
    warnings.extend(report.new_warnings.iter().cloned().map(Warning::from));
    let mut result = write_result(&report)?;
    result["changes"] = json!(changes);
    Ok(Output::Json { result, warnings })
}

fn read_edits(path: &Path) -> Result<Vec<Value>, Failure> {
    let raw = std::fs::read(path).map_err(|e| Error::io(path, &e))?;
    let text = raw.strip_prefix(b"\xef\xbb\xbf").unwrap_or(&raw);
    let value: Value = serde_json::from_slice(text)
        .map_err(|e| Error::invalid(format!("{}: cannot read edit list ({e})", path.display())))?;
    match value {
        Value::Array(ops) if ops.iter().all(Value::is_object) => Ok(ops),
        _ => Err(Error::invalid(format!("{}: expected a JSON array of edit objects", path.display())).into()),
    }
}

fn read(file: &Path, output: Option<&Path>, force: bool, scaffold: bool, no_header: bool, format: Format) -> Outcome {
    let arc = read_archive(file)?;
    let doc = document(&arc)?;
    let view = views::read_view(&arc, &doc, scaffold, if format == Format::Text { "⏎" } else { "\n" });
    let Some(out) = output.filter(|p| p.as_os_str() != "-") else {
        return match format {
            Format::Text => Ok(Output::Text(views::render_read(&view, !no_header, &tool()))),
            Format::Json => json_ok(view),
        };
    };
    guard_output(out, force, &[file])?;
    if same_file(out, file) {
        return Err(Failure::new(ErrorCode::OutputExists, "refusing to write the view over the source .dsd"));
    }
    let rendered = match format {
        Format::Text => views::render_read(&view, !no_header, &tool()),
        Format::Json => {
            let mut s = serde_json::to_string(&view).expect("views serialize");
            s.push('\n');
            s
        }
    };
    atomic_write(out, rendered.as_bytes())?;
    json_ok(json!({
        "output": out.display().to_string(),
        "bytes": rendered.len(),
        "format": if format == Format::Text { "text" } else { "json" },
        "rows": view.rows.len(),
        "tables": view.tables,
    }))
}

fn table(file: &Path, numbers: &[usize], spans: bool, format: Format) -> Outcome {
    let doc = document(&read_archive(file)?)?;
    let tables = numbers.iter().map(|&n| views::table_view(&doc, n, spans)).collect::<dsd::Result<Vec<_>>>()?;
    if format == Format::Text {
        return Ok(Output::Text(tables.iter().map(views::render_table).collect()));
    }
    let warnings = tables
        .iter()
        .filter(|t| !t.contains_unverified.is_empty() || !t.inside_unverified.is_empty())
        .map(|t| {
            Warning::new(
                "unverified-table",
                format!("T{}: coordinates may not match DART Editor and edit commands refuse this table", t.index),
            )
        })
        .collect();
    Ok(Output::Json { result: json!({ "tables": tables }), warnings })
}

fn check(file: &Path, against: Option<&Path>, schema_dir: Option<&Path>, no_schema: bool) -> Outcome {
    let arc = read_archive(file)?;
    let (mut issues, doc) = check_archive(&arc);
    if let Some(doc) = &doc
        && !no_schema
    {
        issues.extend(dsd::schema::check(doc, schema_dir)?);
    }
    // A document that did not parse already has its structural errors; there
    // is nothing to compare, and the issues must still reach the result.
    if let Some(original) = against
        && doc.is_some()
    {
        issues.extend(compare_archives(&read_archive(original)?, &arc)?);
    }
    let count = |level: Level| issues.iter().filter(|i| i.level == level).count();
    let result = json!({
        "file": arc.file_name(),
        "passed": !has_errors(&issues),
        "counts": { "error": count(Level::Error), "warning": count(Level::Warning), "info": count(Level::Info) },
        "issues": issues,
    });
    if has_errors(&issues) {
        let message = format!("{} error-level issue(s) found", count(Level::Error));
        return Err(Failure::new(ErrorCode::CheckFailed, message).with_result(result));
    }
    json_ok(result)
}

fn compare(
    base: &Path,
    other: &Path,
    format: CompareFormat,
    output: Option<&Path>,
    force: bool,
    all: bool,
    limit: usize,
) -> Outcome {
    let (ba, oa) = (read_archive(base)?, read_archive(other)?);
    let (bd, od) = (document(&ba)?, document(&oa)?);
    let cmp = dsd::compare::compare(&ba.file_name(), &bd, &oa.file_name(), &od);
    let output = output.filter(|p| p.as_os_str() != "-");
    if format == CompareFormat::Html && output.is_none() {
        return Err(Error::invalid("--format html writes a file; pass -o PATH")
            .with_hint("for example: dsd compare a.dsd b.dsd --format html -o compare.html")
            .into());
    }
    let listed: Vec<&dsd::compare::Item> =
        cmp.items.iter().filter(|i| all || i.status != dsd::compare::Status::Same).collect();
    let limited = listed.len() > limit;
    let shown = &listed[..listed.len().min(limit)];
    let rendered = match format {
        CompareFormat::Html => dsd::compare::render_html(&cmp, &bd, &od),
        CompareFormat::Text => dsd::compare::render_text(&cmp, shown),
        CompareFormat::Json => {
            let result = json!({
                "base": cmp.base,
                "other": cmp.other,
                "summary": cmp.summary,
                "limited": limited,
                "items": shown,
            });
            if output.is_none() {
                return json_ok(result);
            }
            let mut s = serde_json::to_string(&result).expect("comparisons serialize");
            s.push('\n');
            s
        }
    };
    let mut rendered = rendered;
    if limited && format == CompareFormat::Text {
        let _ = writeln!(rendered, "... {} more item(s) (raise --limit)", listed.len() - limit);
    }
    let Some(out) = output else {
        return Ok(Output::Text(rendered));
    };
    guard_output(out, force, &[base, other])?;
    if same_file(out, base) || same_file(out, other) {
        return Err(Failure::new(ErrorCode::OutputExists, "refusing to write the comparison over a compared .dsd"));
    }
    atomic_write(out, rendered.as_bytes())?;
    json_ok(json!({
        "output": out.display().to_string(),
        "bytes": rendered.len(),
        "format": match format { CompareFormat::Html => "html", CompareFormat::Text => "text", CompareFormat::Json => "json" },
        "summary": cmp.summary,
        "limited": limited && format != CompareFormat::Html,
    }))
}

fn block_json(b: &Block) -> Value {
    json!({ "locator": b.locator, "tag": b.tag, "text": b.text })
}

fn attr_changes(o: &Block, n: &Block) -> Vec<(String, Option<String>, Option<String>)> {
    let od: BTreeMap<&String, &String> = o.attrs.iter().map(|(k, v)| (k, v)).collect();
    let nd: BTreeMap<&String, &String> = n.attrs.iter().map(|(k, v)| (k, v)).collect();
    let keys: std::collections::BTreeSet<&String> = od.keys().chain(nd.keys()).copied().collect();
    keys.into_iter()
        .filter(|k| od.get(k) != nd.get(k))
        .map(|k| (k.clone(), od.get(k).map(|v| (*v).clone()), nd.get(k).map(|v| (*v).clone())))
        .collect()
}

fn diff(original: &Path, edited: &Path, limit: usize, raw: bool, format: Format) -> Outcome {
    let (orig, new) = (read_archive(original)?, read_archive(edited)?);
    let (odoc, ndoc) = (document(&orig)?, document(&new)?);
    let changes = block_diff(&odoc, &ndoc);
    let ranges = raw_line_changes(orig.contents()?, new.contents()?);
    let members: Vec<Issue> = compare_members(&orig, &new);
    let (olines, nlines) = (split_lines(orig.contents()?), split_lines(new.contents()?));
    let raw_lines: Vec<(char, usize, String)> = if raw {
        ranges
            .iter()
            .take(limit)
            .flat_map(|&(i1, i2, j1, j2)| {
                let old = (i1..i2).map(|k| ('-', k, String::from_utf8_lossy(olines[k - 1]).into_owned()));
                let new = (j1..j2).map(|k| ('+', k, String::from_utf8_lossy(nlines[k - 1]).into_owned()));
                old.chain(new).collect::<Vec<_>>()
            })
            .collect()
    } else {
        Vec::new()
    };
    if format == Format::Text {
        let mut out = String::new();
        for c in changes.iter().take(limit) {
            match (c.kind, &c.old, &c.new) {
                (ChangeKind::Changed, Some(o), Some(n)) => {
                    let mut parts = Vec::new();
                    if o.text != n.text {
                        parts.push(format!("{} -> {}", quote(&o.text), quote(&n.text)));
                    }
                    for (k, a, b) in attr_changes(o, n) {
                        let show = |v: Option<String>| quote(v.as_deref().unwrap_or("(none)"));
                        parts.push(format!("@{k} {} -> {}", show(a), show(b)));
                    }
                    if o.tag != n.tag {
                        parts.push(format!("<{}> -> <{}>", o.tag, n.tag));
                    }
                    let _ = writeln!(out, "~ {} -> {}  {}", o.locator, n.locator, parts.join("  "));
                }
                (_, Some(o), None) => {
                    let _ = writeln!(out, "- {} {} {}", o.locator, o.tag, quote(&o.text));
                }
                (_, _, Some(n)) => {
                    let _ = writeln!(out, "+ {} {} {}", n.locator, n.tag, quote(&n.text));
                }
                _ => {}
            }
        }
        if changes.len() > limit {
            let _ = writeln!(out, "... {} more change(s) (raise --limit)", changes.len() - limit);
        }
        let _ = writeln!(out, "{} changed block(s) in cells and text blocks", changes.len());
        let spots = ranges
            .iter()
            .take(20)
            .map(|&(_, _, j1, j2)| if j2 - j1 > 1 { format!("{j1}-{}", j2 - 1) } else { j1.to_string() })
            .collect::<Vec<_>>()
            .join(", ");
        let at = if ranges.is_empty() { String::new() } else { format!(" at edited lines {spots}") };
        let _ = writeln!(out, "contents.xml: {} raw line(s) changed{at}", changed_line_count(&ranges));
        for (side, k, text) in &raw_lines {
            let _ = writeln!(out, "  {side}{k:>6} {text}");
        }
        for issue in &members {
            let _ = writeln!(out, "{}: {}", issue.code, issue.message);
        }
        return Ok(Output::Text(out));
    }
    let shown: Vec<Value> = changes
        .iter()
        .take(limit)
        .map(|c| {
            let mut v = json!({ "kind": match c.kind {
                ChangeKind::Changed => "changed",
                ChangeKind::Removed => "removed",
                ChangeKind::Added => "added",
            }});
            if let Some(o) = &c.old {
                v["old"] = block_json(o);
            }
            if let Some(n) = &c.new {
                v["new"] = block_json(n);
            }
            if let (Some(o), Some(n)) = (&c.old, &c.new) {
                let attrs: Vec<Value> =
                    attr_changes(o, n).into_iter().map(|(k, a, b)| json!({"name": k, "old": a, "new": b})).collect();
                if !attrs.is_empty() {
                    v["attributes"] = json!(attrs);
                }
            }
            v
        })
        .collect();
    let mut contents = json!({
        "changedLines": changed_line_count(&ranges),
        "ranges": ranges.iter().map(|&(i1, i2, j1, j2)| json!({"oldStart": i1, "oldEnd": i2, "newStart": j1, "newEnd": j2})).collect::<Vec<_>>(),
    });
    if raw {
        contents["lines"] = json!(
            raw_lines.iter().map(|(s, k, t)| json!({"side": s.to_string(), "line": k, "text": t})).collect::<Vec<_>>()
        );
    }
    let warnings = members.iter().filter(|i| i.level == Level::Warning).cloned().map(Warning::from).collect();
    Ok(Output::Json {
        result: json!({
            "changeCount": changes.len(),
            "limited": changes.len() > limit,
            "changes": shown,
            "contents": contents,
            "members": members,
        }),
        warnings,
    })
}
