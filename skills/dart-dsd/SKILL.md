---
name: dart-dsd
description: "Read, review, and edit Korean DART Editor .dsd documents (감사보고서, 검토보고서, 사업·분기·반기보고서 drafts) without corrupting them. Use when a task involves a .dsd file: converting it to Markdown, finding or comparing figures, updating table cells or text, adding or removing rows, or verifying an edited .dsd before it returns to DART Editor. Not for .drt submission packages, OpenDART API downloads, or operating the DART Editor GUI."
---

# DART DSD

A `.dsd` is a ZIP holding `contents.xml` (the document), `meta.xml` (editor
metadata and image list), and embedded images. DART Editor remains the
authority for layout and filing; the goal here is to read the document
faithfully and change only what was asked, leaving every other byte as DART
Editor wrote it.

Requires Python 3.9+. All mechanics go through the bundled, standard-library
helper `scripts/dsd.py` (path relative to this skill). Run
`python scripts/dsd.py <command> --help` for exact options. Optional schema
validation additionally uses `lxml` and a local DART schema directory
(default `C:\DART\Resource\Dtd`).

Consult the [format reference](references/format.md) when content is
unfamiliar, when a change needs hand-editing of `contents.xml`, or when
`check` reports a warning you need to interpret.

## Treat documents as confidential

DSD files are usually unreleased client filings. Process them locally. Do not
upload them or their text to external services, commit them to repositories,
or copy them into shared locations. When only structure matters, use
`inspect`, which reports no document text. Put converted Markdown and edited
copies where the user asked, or beside the source.

## Read

1. `inspect FILE` confirms the archive is a DSD and reports schema, editor and
   form versions, structure counts, and issues.
2. `outline FILE` lists sections, filled insertions, images, and tables as
   `T1`, `T2`, … with `contents.xml` line numbers.
3. Choose the view the task needs:
   - `read FILE -o OUT.md` renders Markdown: headings follow `SECTION-n`
     levels, span tables become HTML, `&cr;` becomes a line break, images go
     to `OUT.assets/`, and each table is tagged `<!-- dsd:table T# … -->`.
   - `table FILE N …` prints exact cell text with `R#`/`C#` coordinates,
     spans, row locks, and `ACODE`/`AUNIT` metadata.
   - `find FILE TEXT` locates text across cells and paragraphs, ignoring
     whitespace differences unless `--exact`.

Quote figures from `table` output when precision matters; Markdown collapses
spacing. Editor guidance (`◆click◆` comments, `LIBRARYLIST`, `WARNING`) and
unfilled `INSERTION` slots are not document content.

## Edit

Write every edit to a new file such as `<name>_수정.dsd`. Overwrite the
original only when the user explicitly authorizes it; the helper refuses to
replace an existing file without `--force`.

Never re-serialize `contents.xml` with an XML library or re-zip the archive
with a generic tool. Those rewrite CP949 image names, quoting, empty elements,
and line endings, which DART Editor may then mis-handle. Use the helper's
edit commands, or `extract` → change `contents.xml` as text → `pack`.

1. **Locate.** Use `find` and `table` to get `T#`/`R#`/`C#` and confirm the
   cell holds what the user means. The same figure often appears in several
   statements, notes, and periods; identify each intended occurrence.
2. **Choose the operation.**
   - `set-cell` replaces one cell's text, keeping its `P`/`SPAN` wrapper and
     attributes. Newlines become `&cr;`. A `TU` cell with an `AUNITVALUE`
     requires `--unit-value` in the matching machine form (dates
     `YYYYMMDD`); a date that disagrees with the text is refused. Cells with
     mixed or multi-paragraph content are refused; hand-edit them, or use
     `--flatten` only when losing that formatting is acceptable.
   - `replace --old A --new B` changes document text within a single text
     run. It never matches tags, attribute values, comments, entity
     references, or editor scaffolding, and cannot match across `SPAN`/`P`
     boundaries. It fails unless the occurrence count equals `--expect`
     (default 1). Run it with `--dry-run` first to see every occurrence in
     context. That count, not `find`'s whitespace-insensitive one, is what
     `--expect` must equal. Scope it with `--table N`. Hits inside `TU` cells
     change only the display text; follow up with `set-cell --unit-value`.
   - `copy-row` (optionally `--clear`) and `delete-row` honor the form's row
     locks (`ACOPY`/`ADELETE="N"`) and refuse rows inside a `ROWSPAN`.
     Override locks with `--force-locked` only when the user wants to depart
     from the form.
   - `apply FILE EDITS.json -o OUT` runs a JSON array of those operations in
     order and writes one file. `apply --help` lists each operation's fields
     (they mirror the command flags, e.g. `{"op": "set-cell", "table": 3,
     "row": 4, "col": 2, "text": "1,100,000"}`). Row numbers in later
     operations refer to the table after earlier operations, so list row
     insertions and deletions bottom-up or account for the shift.
   - For anything else, such as new paragraphs or sections, column changes,
     or span changes, `extract FILE DIR`, edit `DIR/contents.xml` as UTF-8
     without BOM keeping CRLF, follow the format reference's size and TOC
     rules, then `pack DIR FILE -o OUT`.
3. **Match the form's conventions.** Format values like their neighbors:
   thousands separators, `(1,234)` negatives, `-` for nil, leading-space
   indentation, and spacing in titles. A value the user spelled out exactly
   is written as given; mention any convention it departs from. Keep
   `ACODE`, `AUNIT`, `USERMARK`, sizes, and `DOCUMENT-HEADER`/`meta.xml`
   values unless the request concerns them. Pass values that start with `-`
   normally (`--text -1,000` works).
4. **Carry related figures.** The helper does not recompute totals or
   cross-references. A changed figure can affect subtotals, totals, other
   statements, and notes. A changed period can affect the other period
   bound, `제 N 기` labels, and dates written in paragraphs. Update those
   too when the request covers them. Otherwise, list them for the user as
   unchanged dependents.

Every edit command re-checks the result. It writes nothing if the change
would add a structural error or turn CRLF line endings into LF.

## Verify and report

1. Run `check OUT --against ORIGINAL`. It checks structure, image and
   `meta.xml` consistency, TOC numbering, table grids, size conventions,
   `TU` date values, and line endings. It also validates against the DART
   schema when `lxml` and a schema directory (`--schema-dir`,
   `DART_SCHEMA_DIR`, or `C:\DART\Resource\Dtd`) are available, and
   otherwise reports `schema-skipped`.
2. Run `diff ORIGINAL OUT`. It lists changed cells and text blocks,
   including attribute changes such as `@AUNITVALUE`, then the count and
   location of changed raw `contents.xml` lines and any other changed
   member. Confirm that every change was intended. If the raw line count
   exceeds what the block list explains, inspect those lines with `--raw`.
3. Report the output path, the changes made, any dependents left unchanged,
   the checks run (with schema validation stated as done or skipped), and
   anything refused or deferred. Opening the file in DART Editor and running
   its own validation before creating a `.DRT` is the user's final check. Do
   not claim DART Editor acceptance unless the file was actually opened there.

Reading is complete when the requested content has been delivered from the
helper's output, with locators where the user needs to act on it. Editing is
complete when the new file exists, `check` has no errors, and `diff` shows
exactly the intended changes.
