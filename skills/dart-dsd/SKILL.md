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
   - `read FILE` lists every text block in order, one per line: the
     `contents.xml` line, the element path from the innermost section
     (`SECTION-2/P`), and the exact text as a JSON string with `⏎` for DART's
     `&cr;` line break. Tables appear as `T#` stubs; images by member name.
     An element inside text other than `SPAN`/`A` stays visible as
     `⟦TAG⟧…⟦/TAG⟧`. `COMMENT`, `LIBRARYLIST`, and `FILENAME` scaffolding is
     hidden unless `--scaffold`; `WARNING` lines are shown. Use `-o FILE` for
     long documents.
   - `table FILE N …` prints exact cell text with `R#`/`C#` coordinates,
     spans, row locks, and `ACODE`/`AUNIT` metadata.
   - `find FILE TEXT` locates text across cells and paragraphs, ignoring
     whitespace differences unless `--exact`.

Both `read` and `table` keep text exactly as stored, so quote figures and
titles from them. When the user wants Markdown or another formatted
rendering, write it yourself from that output: section levels come from
`SECTION-n`, cells from `table`, and images from `extract`. Editor guidance
(`◆click◆` comments, `LIBRARYLIST`, `WARNING`) and unfilled `INSERTION` slots
are not document content.

The helper's rules were verified only on elements and schemas seen in real
DART Editor output, plus a few deeper-level elements (`SECTION-4` to
`SECTION-7`, `TFOOT`) whose schema definition matches a seen one; the
[format reference](references/format.md) lists both. The helper shows
anything else as found but does not interpret it: `inspect`/`check`
report `unverified-element` or `unverified-schema`; `read` lists them in its
`# unverified` header and gives each such element its own line with its
attributes (`NOTE[ACLASS=…]`), or a `⟦TAG[…]⟧` marker when it sits inside text;
`find` tags hits `[unverified …]`; and `table` warns that coordinates may not
match DART Editor. When such content matters to the
task, read the raw XML at the reported line (`extract`) and the
[format reference](references/format.md) before relying on its meaning, and
tell the user which parts were unverified.

## Edit

Write every edit to a new file such as `<name>_수정.dsd`. Overwrite the
original only when the user explicitly authorizes it; the helper refuses to
replace an existing file without `--force`.

Never re-serialize `contents.xml` with an XML library or re-zip the archive
with a generic tool. Those rewrite CP949 image names, quoting, empty elements,
and line endings, which DART Editor may then mis-handle. Two routes avoid
that, and both end in the same verification:

- **Edit commands** (`set-cell`, `replace`, `copy-row`, `delete-row`,
  `apply`) locate and splice the change for you. They refuse when the edit's
  meaning depends on something unverified: a table that contains or sits
  inside an unverified element, a `replace` hit inside one, or a file with an
  unverified schema.
- **Text editing**: `extract FILE DIR`, edit `DIR/contents.xml` as text,
  then `pack DIR FILE -o OUT`. Use it whenever a command refuses or the
  change is outside the commands' scope (new paragraphs or sections, column
  or span changes, unverified content). Read the surrounding XML first and
  follow the format reference's text conventions and size and TOC rules:
  UTF-8 without BOM, CRLF kept, `&cr;` for line breaks. `pack` restores the
  original archive layout and refuses structural damage, so this route is as
  safe for the container as the commands are.

1. **Locate.** Use `find` and `table` to get `T#`/`R#`/`C#` and confirm the
   cell holds what the user means. The same figure often appears in several
   statements, notes, and periods; identify each intended occurrence.
2. **Choose the operation.**
   - `set-cell` replaces one cell's text, keeping its `P`/`SPAN` wrapper and
     attributes. Newlines become `&cr;`. A `TU` cell with an `AUNITVALUE`
     requires `--unit-value` in the matching machine form (dates
     `YYYYMMDD`); a date that disagrees with the text is refused. Cells with
     mixed or multi-paragraph content are refused; hand-edit them, or use
     `--flatten` only when losing that formatting is acceptable. A position
     covered by another cell's `ROWSPAN`/`COLSPAN` is refused; the error
     names the spanning cell to address instead.
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
   - For anything else, or when a command refuses, use the text-editing
     route above. Do not reach for `--flatten` or `--force-locked` just to
     get past a refusal.
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
   bound, its repeated `PERIODFROM2`/`PERIODTO2` copy, `제 N 기` labels, and
   dates written in paragraphs. Update those too when the request covers
   them. Otherwise, list them for the user as unchanged dependents.

Every edit command re-checks the result. It writes nothing if the change
would add a structural error or turn CRLF line endings into LF.

## Verify and report

1. Run `check OUT --against ORIGINAL`. It checks structure, image and
   `meta.xml` consistency, TOC numbering, table grids, size conventions,
   `TU` date values and repeated period copies, and line endings. It also
   validates against the DART schema when `lxml` and a schema directory
   (`--schema-dir`, `DART_SCHEMA_DIR`, or `C:\DART\Resource\Dtd`) are
   available, and otherwise reports `schema-skipped`.
2. Run `diff ORIGINAL OUT`. It lists changed cells and text blocks,
   including attribute changes such as `@AUNITVALUE`, then the count and
   location of changed raw `contents.xml` lines and any other changed
   member. Confirm that every change was intended. If the raw line count
   exceeds what the block list explains, inspect those lines with `--raw`.
3. Report the output path, the changes made, any dependents left unchanged,
   the checks run (with schema validation stated as done or skipped), and
   anything refused or deferred, and any skill problem worth reporting (see
   below). Opening the file in DART Editor and running
   its own validation before creating a `.DRT` is the user's final check. Do
   not claim DART Editor acceptance unless the file was actually opened there.

Reading is complete when the requested content has been delivered from the
helper's output, with locators where the user needs to act on it. Editing is
complete when the new file exists, `check` has no errors, and `diff` shows
exactly the intended changes.

## Report skill problems

This skill and its helper are maintained at
<https://github.com/cpaikr/dsdai>. Propose an issue there when a task exposes
a defect or avoidable friction in the skill itself:

- the helper crashes, hangs, or writes a file that `check` or DART Editor
  rejects;
- its output contradicts the XML or DART Editor: missing or altered text,
  `T#`/`R#`/`C#` coordinates that land on another cell, a wrong `diff`;
- a refusal or `unverified-*` report on content DART Editor routinely
  produces, forcing the text-editing route;
- guidance here that was wrong or missing, or that cost avoidable detours.

Errors in the document, DART Editor's own behavior, and tasks this skill
excludes are not skill issues. Work around the problem and finish the user's
task first; raise the issue in the final report.

The repository is public, so an issue carries no document content: no text,
figures, company or person names, file or image member names, or data values
such as `AUNITVALUE`. Describe structure instead: the command with placeholder
paths (`in.dsd`), the issue code and message with document text replaced by
`…`, element tags and attribute names, the `meta`/`doc` version lines from
`inspect`, `dsd.py --version`, Python version, and OS. When a reproduction
helps, write a minimal `contents.xml` fragment with invented text. State
expected and actual behavior and the workaround used.

Search existing issues first (`gh issue list --repo cpaikr/dsdai --state all
--search "<keywords>"`) and prefer a comment on a match over a new issue.
Filing publishes, so show the user the drafted title and body, and file only
after they approve: `gh issue create --repo cpaikr/dsdai --title "<title>"
--body-file <draft> --label <bug|enhancement|documentation>`. Without `gh` or
GitHub access, give the user the draft and
<https://github.com/cpaikr/dsdai/issues/new>. Report the resulting URL.
