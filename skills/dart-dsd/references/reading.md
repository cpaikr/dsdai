# Read a document

1. `dsd inspect FILE` confirms the archive is a DSD and reports schema, editor
   and form versions, structure counts, and issues, without document text.
2. `dsd outline FILE` lists sections, filled insertions, images, and tables
   (`T#`) with line numbers; `--all` adds paragraphs and empty insertions.
3. Choose the view the task needs:
   - `dsd read FILE` lists every text block in order with its line, element
     path from the innermost section (`SECTION-2/P`), and exact text. JSON
     gives DART's `&cr;` line break as `\n`; `--format text` quotes each block
     as a JSON string with `⏎`. Tables appear as stubs (`table.index`); images
     by member name. An element inside text other than `SPAN`/`A` stays
     visible as `⟦TAG⟧…⟦/TAG⟧`. Editor scaffolding (`COMMENT`, `LIBRARYLIST`,
     `FILENAME`) is hidden unless `--scaffold`; `WARNING` lines are shown.
     For a long document, write the view to a file with `-o`.
   - `dsd table FILE N …` gives cells with `R#`/`C#`, spans, row locks
     (`acopy`/`adelete`), and `ACODE`/`AUNIT`/`AUNITVALUE`. Cell text is
     normalized for reading: `&cr;` becomes a line break, trailing whitespace
     and blank first or last lines are trimmed, and whitespace-only paragraphs
     are dropped; leading indentation is kept.
   - `dsd find FILE TEXT` locates text in cells and paragraphs, ignoring
     whitespace differences unless `--exact`; `result.limited` means more
     matches exist than `--limit`.

`read` keeps text exactly as stored and `table` keeps every character that
carries content, so quote figures and titles from them; use `read` or
`extract` when trailing or whitespace-only text matters.

## Markdown and other renderings

The tool emits no Markdown; write it from the views. Section levels come from
`SECTION-n`, cells from `table` (keep row and column spans, for example as
HTML tables), line breaks from `&cr;`, and images from `dsd extract`. Omit
editor guidance and unfilled insertions.

## Unverified content

The tool's rules cover only elements and schemas seen in real DART Editor
output (plus a few with identical schema definitions); the
[format reference](format.md) lists them. Anything else is shown as found but
not interpreted: `inspect`/`check` report `unverified-element` or
`unverified-schema`; `read` lists it in `result.unverified` and gives each
such element its own row with its attributes, or a `⟦TAG[…]⟧` marker inside
text; `find` matches carry `unverified`; and `table` warns that coordinates
may not match DART Editor. When such content matters, read the raw XML at the
reported line (`dsd extract`) and the format reference before relying on its
meaning, and tell the user which parts were unverified.

## Compare two documents

- `dsd diff ORIGINAL EDITED` is for one file and its edit: blocks pair by
  position, rows before cells, so an inserted or deleted row is reported whole
  at its coordinates. It lists changed cells and text blocks (including
  attribute changes such as `AUNITVALUE`), then the changed raw `contents.xml`
  line ranges and any other changed member; `--raw` adds the lines.
- `dsd compare BASE OTHER` is for two versions that may differ in form or
  structure: a draft against the filed report, this quarter against the last.
  Tables and paragraphs pair by content (a reordered table is paired and
  marked `moved`; a table inside a cell is compared as a table of its own),
  rows by their label (footnote markers and 주식회사/㈜ spellings ignored,
  moved rows detected), and columns by header when the counts differ. Each item is `same`, `changed`, `onlyBase`,
  or `onlyOther` under the base document's section (note) heading; cell
  changes are `value` (with `delta` = OTHER − BASE), `text`, or `format` (the
  same number or label written differently). JSON lists only differences
  unless `--all`.
- For a person, write the side-by-side report:
  `dsd compare BASE OTHER --format html -o compare.html`. It renders each
  document in its own pane at DART's sizes, aligned item by item and
  scrolling together, highlights changed cells and words, and has a change
  map, a section index, next/previous navigation, a changes-only filter, and
  a panel listing the selected item's changes. It is one local file; keep it with the documents and
  do not upload it. Summarize what matters from `--format text`.
