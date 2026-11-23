# Plan: xlsx export and footing

Two features, built in order. Phase 2 does not start until Phase 1 passes its
acceptance gate and the owner signs off.

1. **`dsd xlsx`** — convert a `.dsd` into an `.xlsx` workbook. Fully
   deterministic; no judgment involved.
2. **`dsd foot`** — footing: sum checks and cross-reference checks over the
   same document. The CLI proposes and evaluates checks deterministically; an
   agent contributes only the judgment (which cells should relate), as data.

This file owns the plan and its progress. It holds no sample content: the
confidentiality rules in [AGENTS.md](AGENTS.md) apply to every example here.

## Status

| Item | State |
|---|---|
| Branch | `feat/footing` (Phase 1 merged to `main` with PR #18) |
| Phase 1 | Done. Gate passed; the owner reviewed the working sample in Excel and signed off (2026-11-13) |
| Phase 2 | Done (owner signed off 2026-11-23). Branch `feat/footing`, not yet in a PR |

Update this table and the step checkboxes as work lands. Record decisions made
along the way under [Decisions](#decisions), not in commit messages only.

## Decisions

Settled with the owner (2026-11-12):

- Phase 1 first; Phase 2 only after Phase 1 is confirmed working.
- The workbook has an index sheet plus one sheet per statement and per note.
  There is no "whole document on one sheet" copy.
- The workbook is one-way output for reading and checking. Nothing is read
  back from xlsx into a `.dsd`; corrections go through the existing edit
  commands.
- The non-deterministic part of footing is **CLI feature + skill guidance**,
  not skill only: the agent writes a checks file, the CLI does all arithmetic
  and renders the result.
- Cross-reference scope for Phase 2 is the document itself: statement ↔ note,
  note ↔ note, and current-period opening ↔ prior-period closing. Comparing
  with last year's filing stays with `dsd compare`. Reconciling with a
  client's Excel package is a separate, later project.
- Target documents: audit and review reports (separate and consolidated) that
  carry statements and notes. Other document types must still convert
  completely, with a plainer sheet split.

Defaults chosen by the implementer; the owner can veto any of them before
step 1.2:

- Command names `xlsx` and `foot`.
- Implementation in Rust inside the existing crates, with `rust_xlsxwriter`
  as the writer (0.102, MIT/Apache-2.0, MSRV 1.88 — equal to the workspace
  MSRV) and `calamine` as a test-only reader. Owning an xlsx serializer would
  be durable complexity the project should not carry.
- Sheet names: index sheet `총괄표`; statements by their own compacted title
  (for example `재무상태표`, `반기연결포괄손익계산서`); notes `주석1`, `주석2`, …;
  other regions by their section title.
- A nil dash (`-`) becomes the number 0 displayed as `-`.
- Each table gets a small grey `T#` marker beside its first row.

Recorded during implementation:

- **Determinism (1.1).** `DocProperties::set_creation_datetime` pins the
  creation time, and the modified time follows it; the writer's zip layer
  already uses a fixed member timestamp. A probe workbook (styles, merges,
  links, defined names, Korean shared strings) was byte-identical across
  three separate processes. The export pins 2000-01-01T00:00:00Z and leaves
  author and company empty.
- **Compression backend (1.1).** `rust_xlsxwriter` depends on `zip` with
  its `deflate` feature, which enables `flate2/zlib-rs`; flate2 then prefers
  zlib-rs over miniz_oxide for the whole workspace. There is no feature to
  opt out. Consequence: a member that an edit recompresses (`contents.xml`)
  gets different, still deterministic, deflate bytes than v0.1.2 wrote; the
  decompressed bytes and every unchanged member's compressed bytes are as
  before, so the byte-preservation invariant holds. The committed public
  fixture was regenerated for the same reason (identical member contents).
  zlib-rs is pure Rust (Zlib license), so no C toolchain is added.
- **Size and licenses (1.1).** The writer adds about 1.75 MB to the
  release executable (Windows: 7.06 MB at v0.1.2, 8.81 MB with the export). New licenses are MIT, Apache-2.0, Zlib,
  and CC0/MIT-0 alternatives; `scripts/release.py` ships only `LICENSE.md`
  and needs no change. `calamine` is a dev-dependency only.
- **Partition details (1.2).** A `PART`/`SECTION-1`/`SECTION-2` sheet whose
  title passes the statement-title test is kind `statement`, not `section`
  (so the index labels it correctly). A nested `SECTION-2` keeps the note
  numbering of its parent, as `compare` does. The region's own `TITLE`
  stays a bold content row as well as the row-1 sheet title, so content rows
  hold every text line. Sheets that would be empty are dropped. Names are
  compared case-insensitively (as Excel does), `History` is reserved, and
  every name that differs from its title (sanitized, shortened, or numbered)
  gets a `sheet-name-changed` warning. Excel's 32,767-character cell limit
  fails the export with `invalid_argument`, like the row and column limits.
- **Layout details (1.2).** Index sheet: row 1 title, row 3 headers
  (`시트`, `구분`, `제목`), one row per sheet from row 4, rows 1–3 frozen;
  `구분` shows Korean kind labels (`표지`, `목차`, `본문`, `섹션`, `재무제표`,
  `주석`). An image row reads `[이미지] <member>`. A table that cannot be
  mapped (oversized or without a grid) gets one grey `[T#] …` line and a
  `table-not-mapped` warning. `COL` widths are clamped to 24–360 px.
  A spanning cell merges only when it owns every grid position of its span;
  an overlapping (malformed) span is written unmerged. Text with line breaks
  wraps.
- **Typing details (1.3).** Whitespace is ignored only around the figure
  and between a sign or parenthesis and its digits (`( 1,234 )`, `▲ 1,234`);
  whitespace between digits keeps a cell text. A negative zero (`(0)`, `-0`)
  stays text because Excel would show `0`. Only the ASCII hyphen counts as a
  nil dash or minus sign. Percentages are parsed by moving the decimal point,
  so `12.5%` is exactly the double nearest 0.125. The nil-dash format is
  `#,##0;-#,##0;"-"`, so a value typed over it later still reads as an amount.
- **Writer details (1.4).** Bounds are checked while building the model
  (`sheets.rs`), so the writer only maps roles to styles; any writer error
  that still occurs is reported as `invalid_argument`. Sheet names are always
  single-quoted in defined names and internal links. Header cells are bold,
  centred both ways, and filled `#D9D9D9`; markers are 8 pt grey `#808080`.
- **CLI details (1.5).** `-o -` is refused with `invalid_argument` (a
  workbook never goes to stdout). `counts.numberCells`/`textCells` count
  table cells only. `sheets[].rows` is the last used row.
- **Text lines keep SPAN edges (1.7).** The corpus check found paragraphs
  whose line break is a `&cr;` alone in a SPAN. `node_text` trims each
  non-block child on its own, so that break (and a trailing space inside a
  SPAN) is lost and two lines join. Text lines now use
  `block_text_outside_tables`, which collects SPAN content in place. Table
  cells keep `node_text`'s rule so they stay equal to `dsd table`; the same
  loss therefore remains in `table`, `find`, `diff`, and `compare` output,
  left unchanged here because it alters existing commands (a follow-up).
- **Review fixes (1.8).** A statement section whose first table repeats its
  title keeps one sheet, named by the compacted title (it used to get an
  extra one-row sheet with the spaced title). A table with grid problems
  gets a `table-grid-problem` warning, since cells outside its declared
  grid are not exported (`dsd table` omits them too). `sheets[].rows`
  counts rows covered only by a span. The corpus check also compares each
  number cell's number format with one derived independently.
- **PR review fixes (after 1.9).**
  - A `SECTION-2` inside a notes region stays a notes region, so its
    paragraphs still open note sheets.
  - A paragraph's tables follow the text segment that holds their placeholder,
    so a table before a later note heading stays on the earlier note.
  - Only a paragraph that wraps a real table is skipped as a wrapper; a
    literal `[표]` is text.
  - Statement-title detection reads the first cell without nested tables.
  - `result.tables` is in document order even when 본문 gathers content from
    both sides of a region.
  - A multi-line merged cell gets the height its lines need, shared evenly
    by the rows it spans, since Excel does not fit merged cells; a share no
    taller than the default 15 pt row is left to Excel. A line is 17 pt
    (Hangul falls back to a taller font than Calibri), and lines are counted
    after estimated wrapping at the merged width (15 px per wide glyph, 8 px
    otherwise); the estimate errs tall.
  - Text lines mark a nested table with a sentinel that XML cannot contain,
    so a literal `[표]` is never taken for a table.
  - Merge membership and merge origins are looked up once, not rescanned.
- **Corpus check (1.7).** Container cells of nested tables are compared by
  their own lines in order, since `dsd table` flattens the nested table into
  the cell. Text lines are compared as a multiset of whitespace-free lines
  against an independent ElementTree extraction that skips the document
  header, scaffolding, and tables. The check also requires byte-identical
  reruns and that every table is mapped or reported unmapped.

Settled with the owner (2026-11-13):

- **No statement sheets inside a notes region.** A statement-named table in
  a note, such as the appropriation statement (`이익잉여금처분계산서`) in the
  retained-earnings note or a `요약재무상태표` summary in a subsidiary or
  associate note, stays on its note's sheet, as the reference workbook
  does. Before this, rule 3 opened a statement sheet there and the rest of
  the note followed it until the next note heading.

## What the reference workbook taught us

A third-party footing workbook for one real document was reviewed locally (it
is not in the repository). Structural lessons only:

- Its layout — index sheet, per-statement sheets, per-note sheets, spans as
  merged cells, amounts as numbers — is what users expect. Phase 1 follows it,
  minus the whole-document sheet.
- It converts a comma-separated list of note numbers into one large number.
  Number detection must validate thousands grouping.
- Every check is a same-sheet `total − SUM(parts)` formula chosen from labels
  alone. Most of its reported errors are false positives: in movement
  schedules it sums the change columns and omits the opening balance.
  A proposer must **verify candidate structures arithmetically** and report
  "no structure holds" as needing judgment, not as an error.
- It has no cross-sheet checks at all, and its per-sheet error counts are
  static values that go stale when a cell is corrected. Phase 2 must provide
  real cross-reference checks and live counts.

## Phase 1 — `dsd xlsx`

### Command

```text
dsd xlsx <FILE.dsd> -o <OUT.xlsx> [--force]
```

- Stdout carries one JSON envelope (the workbook goes to the file). Adding a
  command is additive, so `metadata.contract` stays `1`.
- `-o` is required. The output never replaces an existing file without
  `--force`, and never replaces a source `.dsd` even with `--force` (same rule
  as `read -o` and `compare -o`). It is written through a temporary file and a
  rename.
- Failures use existing codes: `file_not_found`, `invalid_archive`,
  `invalid_xml`, `invalid_argument` (a table or sheet beyond Excel's limits),
  `output_exists`, `io_error`. No new error code is expected.

Result fields:

```json
{
  "output": "report.xlsx", "bytes": 0, "sha256": "…",
  "sheets": [{"name": "재무상태표", "kind": "statement", "title": "…", "rows": 0, "tables": [12, 13, 14]}],
  "tables": [{"table": 13, "sheet": "재무상태표", "range": "A7:F45", "name": "DSD_T13"}],
  "counts": {"sheets": 0, "tables": 0, "numberCells": 0, "textCells": 0}
}
```

Warnings (not failures): unverified elements or schema (content is still
exported as text), a table too malformed to map (exported as a note line
naming it), and sheet names that had to be shortened or numbered.

### Sheet partition

Every body block lands on exactly one sheet, in document order. The editor's
scaffolding and `DOCUMENT-HEADER` are skipped, as in `read`.

A new sheet starts at:

1. `COVER` and `TOC` (sheets `표지`, `목차`).
2. A `PART`, `SECTION-1`, or `SECTION-2` with a non-empty `TITLE`. Deeper
   sections stay on their parent's sheet; their titles become bold rows.
   When a nested section ends, later content of the parent returns to the
   parent's current sheet.
3. **A statement title table**, outside `COVER`/`TOC`: a table whose first
   non-empty cell, with whitespace removed, is at most 20 characters and ends
   with a known statement name (`재무상태표`, `손익계산서`, `자본변동표`,
   `현금흐름표`, `대차대조표`, `이익잉여금처분계산서`, `결손금처리계산서`). This
   covers `포괄손익계산서` and period/consolidation prefixes. The sheet runs to
   the next statement title or the end of its region. Not inside a notes
   region (rule 4): there such a table stays on its note's sheet.
4. **A note heading**, only inside a region whose title contains `주석`: a
   paragraph line that opens a numbered note, using the existing rule in
   `compare.rs` (`note_heading` + `next_note`: short, not a sentence, numbers
   ascending in small steps). A paragraph that holds a heading after a line
   break is split at that line, so the heading opens its own sheet.

Content before any region goes to a `본문` sheet. Sheet names are sanitized
for Excel (no `[]:*?/\`, at most 31 characters) and made unique with a
` (2)` suffix; the index sheet lists each sheet's full title.

Known limit, accepted for Phase 1: a business report with several note
sections produces `주석1`, `주석1 (2)`, … . Revisit only if such documents
become a target.

### Sheet layout

- Row 1: the sheet's title in bold. Row 2: a link back to `총괄표`. Rows 1–2
  are frozen. Content starts at row 4.
- A text block is one row per line, text in column A (DART line breaks split
  rows; empty blocks are dropped). `TITLE` and `COVER-TITLE` are bold. An
  image is a row naming its archive member; images are not embedded.
- A table occupies its grid: cell (R, C) of the table is at a fixed offset
  from the table's top-left cell, spans become merged ranges, and one blank
  row separates it from neighbours. Header cells (`TH`) are bold, centred,
  and shaded; tables with `BORDER="1"` get thin borders.
- A table nested in a cell follows its container as its own table; the
  container cell shows the existing `[표]` placeholder.
- Column widths are per sheet: the widest declared `COL` width among the
  sheet's tables for each column, converted from pixels and clamped.
- Index sheet `총괄표`: one row per sheet with a link, kind, and full title.
  Phase 2 adds check counts beside it.

### Cell typing

The rule is display fidelity: **a cell becomes a number only when its Excel
rendering reproduces the source text** (ignoring whitespace). Everything else
stays text, exactly as `dsd table` reports it, including leading spaces.

| Source form | Value | Number format |
|---|---|---|
| `1,234,567` (valid grouping) | 1234567 | `#,##0` |
| `1234`, `2025` (no grouping) | 1234 | `0` |
| `(1,234)` | −1234 | `#,##0;(#,##0)` |
| `-1,234`, `△1,234`, `▲1,234` | −1234 | negative part keeps the sign glyph |
| `12.50` | 12.5 | decimals kept (`#,##0.00` / `0.00`) |
| `12.5%` | 0.125 | `0.0%` |
| `-` alone | 0 | displays `-` |
| `1,234 (*1)`, `500(주2)` | 1234 | marker kept as a literal: `#,##0" (*1)"` (since 2.2) |

Kept as text: invalid grouping (`5,6,32`), leading zeros, more than 15
significant digits, an explicit `+`, a marker before the figure or any
other text, dates, and empty cells (left empty). Numbers are right-aligned.

Footnote-marked amounts (`1,234 (*1)`) stayed text in Phase 1. Since step
2.2 they are numbers whose format carries the marker (see Phase 2 decisions).

### Provenance

Footing results must lead back to `T#/R#/C#` so a wrong figure can be fixed
with `set-cell`.

- Each table's range is a workbook defined name `DSD_T<n>` (plain `T<n>`
  collides with cell addresses). Because the grid maps one-to-one, the name
  alone locates every cell.
- The envelope's `tables` array carries the same mapping for agents.
- A grey `T<n>` marker sits in the first column right of each table, on its
  first row, for people.

### Architecture

```text
crates/dsd/src/sheets.rs   partition + layout + cell typing → a plain Workbook model
crates/dsd/src/xlsx.rs     Workbook model → xlsx bytes (rust_xlsxwriter; no file I/O)
crates/dsd-cli             `xlsx` command: guards, atomic write, envelope
```

- `.dsd` semantics (what is a statement, a note, a number) live in the
  library, per the repository boundary. The model is the seam Phase 2 builds
  on: checks address `T/R/C`, and the layout turns them into cell references.
- `note_heading` and `next_note` in `compare.rs` become `pub(crate)` and are
  reused, so `compare`'s section index and the sheets agree on what a note is.
  `compare`'s own numeric parser is left alone: its leniency is right for
  deltas and wrong for typing.
- Determinism: fixed document properties (no clock), fixed iteration order,
  no absolute paths or user names in the file.

### Steps

- [x] **1.1 Spike the writer.** Add `rust_xlsxwriter` (library) and
  `calamine` (CLI dev-dependency). Confirm: two runs give identical bytes
  once the creation time is pinned (the API for that is **not yet
  confirmed** — check `DocProperties` in the docs first); the release
  profile's size increase is acceptable; `cargo clippy` with the workspace's
  pedantic lints stays clean; `scripts/release.py` packaging and its tests
  need no change for the new licenses. If determinism cannot be had, stop and
  raise it before going further.
- [x] **1.2 Model and partition** (`sheets.rs`): walker, sheet boundaries,
  name sanitizing, table placement, nested tables. Unit tests for statement
  title detection, note splitting inside and outside a notes region, a
  heading after a line break, and name collisions.
- [x] **1.3 Cell typing** (in `sheets.rs` or a small `amount.rs`): the table
  above as unit tests, plus the refusals (bad grouping, leading zeros, long
  digits).
- [x] **1.4 Writer** (`xlsx.rs`): styles, merges, widths, links, defined
  names, index sheet, bounds checks against Excel's row/column limits.
- [x] **1.5 CLI**: `Command::Xlsx`, handler, help text, `Command::name`.
- [x] **1.6 CLI tests** in `crates/dsd-cli/tests/cli.rs`, against a fixture
  variant that adds a synthetic statement title table and a notes region
  (built in the test with `replace_once`; `fixtures/public/sample/` stays as
  is): sheet names and order, cell values and types, merged ranges, defined
  names, byte-identical reruns, output guards, unverified-element warning.
- [x] **1.7 Corpus smoke** in `scripts/check.py`: export the public fixture
  and each private sample, read the xlsx back with the standard library only
  (`zipfile` + `ElementTree`), and require every origin cell of every table
  to match `dsd table` — text cells exactly, number cells by an independent
  parse of the source text — and every text line to be present. Print sample
  indexes and counts only.
- [x] **1.8 Docs and skill**: README command list and example,
  ARCHITECTURE boundaries, a short export section in the skill's reading
  reference with routing from `SKILL.md`. Then `/code-review` and
  `/harmonize-docs changes`.
- [x] **1.9 Gate**: run the acceptance list below; the owner opens the
  workbook for the working sample in Excel and signs off.

### Acceptance gate

Phase 1 is done when all of these hold:

1. **Complete** — step 1.7 reports zero mismatches on the public fixture and
   every private sample.
2. **Correctly typed** — the typing unit tests pass; note-number lists and
   years survive unchanged.
3. **Deterministic** — the same input yields byte-identical output.
4. **Opens cleanly** — Excel opens the sample workbook with no repair
   prompt; merged cells, links, and frozen rows behave. (Manual, by the
   owner.)
5. **Contract** — one envelope per run, the guards above, no source file
   ever overwritten.
6. `python scripts/check.py` passes.

### Out of scope for Phase 1

Check formulas, embedded images, fonts and colours from `USERMARK`, xlsx →
dsd, and any option flags. Add an option only when a real need appears.

## Phase 2 — `dsd foot`

Design below is the intended shape. Re-read it against what Phase 1 actually
produced, settle the open decisions, and expand the steps before starting.

### One primitive

Every check is `target = Σ sign × term`, over cells addressed by `T/R/C`:

```json
{
  "id": "T13.R3.C4",
  "kind": "vertical | horizontal | movement | cross | identity",
  "origin": "auto | agent",
  "target": {"table": 13, "row": 3, "col": 4, "rowLabel": "…", "colLabel": "…"},
  "terms": [{"sign": 1, "table": 13, "rows": [4, 8], "col": 3}],
  "note": "why this relation holds (agent checks)"
}
```

`kind` only describes; evaluation is the same for all. An equality is a
single term. Labels are stored beside coordinates so a checks file can be
re-applied to a later draft and flagged where the label no longer matches.

### Three layers

| Layer | Who | Does |
|---|---|---|
| Propose | CLI | Deterministic candidates, each verified arithmetically |
| Judge | Agent, via the skill | Adds, replaces, or dismisses checks in a checks file |
| Evaluate and render | CLI | Computes every check; JSON result and a workbook with live formulas |

**Proposers** (CLI, deterministic):

- Vertical totals from labels (`합계`, `소계`, `총계`, `계`) and from statement
  hierarchy (numbering and indentation, including the two-column
  detail/subtotal layout of statements).
- Horizontal totals from header labels.
- Movement schedules: opening + changes = closing.
- Statement identities where labels match a small known set (assets =
  liabilities + equity, and the like).
- Statement ↔ note: for a statement line with note numbers, cells in those
  notes carrying the same amount for both periods.
- Current opening = prior closing between paired period tables.

A proposer may try several structures for one total and keeps the one that
holds. When none holds it reports the total as **unresolved** with its best
candidate — a request for judgment, not an error. Totals nobody could
structure are listed as **uncovered**.

**Checks file** (agent): an overlay holding only additions and, by `id`,
replacements and dismissals with a reason. The agent never computes; it reads
tables with the existing commands, states relations, and reruns.

**Commands**:

```text
dsd foot <FILE.dsd>                                  proposals + results (JSON)
dsd foot <FILE.dsd> --checks checks.json -o out.xlsx  merged result + workbook
```

The workbook is the Phase 1 workbook plus: a formula per check beside or
below its table (zero shown as passed, a difference in red), cross-reference
formulas on their own sheet, each tagged auto or agent, and per-sheet counts
on `총괄표` computed by formula so they follow corrections.

**Skill**: a footing reference (workflow: run → review failed, unresolved,
uncovered → inspect tables → write the overlay → rerun → report true
differences with `T/R/C` and amounts) linked from `SKILL.md`. The agent
reports differences; it changes figures only when asked.

### Decisions (settled with the owner, 2026-11-18)

- **Tolerance:** a check passes only when its difference is exactly 0. A
  check may state its own tolerance (for example ±1 per term in a document
  in thousands), so every allowance is explicit and shown with the result.
- **Units:** the CLI reads each table's unit from its caption
  (`(단위: 원)`, `천원`, `백만원`, in the table or the line just before it)
  and converts before comparing tables in different units. A check whose
  unit cannot be determined is reported as needing judgment, not evaluated.
- **Statement ↔ note:** when a named note holds the same amount for both
  periods, the CLI records an auto check that passes. Anything else is
  offered as a candidate for the agent, never reported as an error.
- **Footnote-marked amounts:** `1,234 (*1)` becomes the number 1234 with a
  number format that shows the marker (`#,##0" (*1)"`), so the cell still
  reads exactly as the source and formulas can use it. This changes Phase 1
  output for those cells; the display-fidelity rule is unchanged.
- **Re-applying a checks file:** a check finds its cells by `T/R/C` first and
  verifies the stored labels there. When they no longer match, the CLI looks
  for the same labels nearby and reports the check as moved or stale instead
  of evaluating the wrong cells.

Recorded during implementation:

- **2.2 scope.** The model and evaluator live in `crates/dsd/src/foot.rs`
  as a library API; the `dsd foot` command arrives with the first proposer
  (2.3), when a run has checks to report.
- **Evaluation rules (2.2).** A blank cell counts as 0 in a sum; a nil dash
  is 0; a text cell in a term, a blank target, or a cell outside its table
  makes the check need judgment, with a reason naming the cell. A spanning
  cell covering several positions of a term is summed once. A per-term
  tolerance allows that amount for every cell the terms sum. Differences
  within 10⁻⁹ of the larger value (binary rounding of decimals) pass.
- **Units (2.2).** A table's unit is its own caption, else the caption in
  the block just before it (a unit line, or a small caption table, which is
  how statements and notes carry it); a caption names its unit after a colon
  (`단위:`). Currency units convert (원, 천원, 백만원, 억원, 십억원); other
  units (주, 명, USD) compare only with the same unit. Some corpus tables
  with figures have no detected unit, so their cross-table checks need
  judgment.
- **Labels (2.2).** A row's label is its first text that is not a figure;
  a column's is its header cells (`TH` rows from the top) joined with ` / `,
  else the first row's text.

- **In-table proposers (2.3).** Each table is read as figures by period
  group: one figure column, or a statement's detail/subtotal pair (two
  adjacent columns under one heading spanning exactly them, never both
  non-zero in a row). Columns of note references (`주석`), rates (`…율`,
  `…률`), prices, averages, issue numbers, dates, and percentages are never
  summed. A row's level is its pair column, then its numbering
  (Ⅰ. > 1. > 가. > (1) > ① > (가)), then indentation.
- **Choosing a structure (2.3).** A target gets candidate structures in a
  fixed order and keeps the one holding in the most period groups, counting
  only groups where something is non-zero; the structure is then applied to
  every group, so a difference in one period fails instead of hiding. A
  labelled total (합계, 소계, 총계, `… 계`, a top-level closing balance) or a
  numbered breakdown that nothing explains is unresolved with the closest
  candidate; a breakdown implied only by indentation, a signed chain of
  statement lines, or an unlabelled total column is kept only when it holds
  with two non-zero parts (a chain in every period, a column in more than
  half its rows). A lone `-`
  under non-zero parts is a total left out, not a check. An opening row
  repeats the closing above it only across a year end.
- **Check ids (2.3).** `v:` (down a column), `h:` (across a row), `i:` (the
  balance identity), or `x:` (across tables or periods; a statement-note
  check adds `/T#` of its note table) with the target `T#.R#.C#`; a repeat gets `#2`.
- **Statement ↔ note (2.3).** A statement table is one the workbook model
  puts on a statement sheet; a note's tables are those on its note sheet. A
  line names its notes in a 주석 column or a `(주4)` label. Its amounts are
  looked for in every figure cell of those notes' tables, converted between
  known units; one note row holding both periods side by side is preferred,
  then a row with the line's label, then a total row. Both periods found:
  checks (auto), as settled. One period (including a line showing only
  one): `candidates`, a new report list, evaluated but never errors. None,
  while the note has tables: uncovered (`cross`). A note the document does
  not hold is skipped; a workbook model that fails (an Excel limit) skips
  the matching with a `statement-notes-skipped` warning.
- **Prior closings (2.3).** Asserted, since an opening balance is the prior
  closing: within a table with a current and a prior period column (named
  by 당기/전기, or a statement's two columns in order), each top-level
  opening row against the next closing row; across tables, a current and a
  prior table with the same figure columns, named by their header or the
  caption just before them (a unit caption between keeps the period), row
  by row or column by column by label (a label repeated on either side, such
  as two 소계 rows, is not paired). Within a table the two periods must be
  named; two unnamed columns of a note are usually two items. A prior interim period (분기, 반기)
  is never compared, since the next period opens from the prior year end.
- **Checks file (2.4).** `dsd foot FILE --checks CHECKS` reads
  `{"checks": [...], "dismiss": [...]}` (both optional, nothing else). A
  check needs `origin: "agent"`, a `note`, at least one term, and no `rule`;
  it replaces the proposal with its id at its target (an unresolved total
  or a candidate included; a replaced check keeps its place) or else adds a
  check. A `#2` id names the second proposal at one cell in emission order. A dismissal is `{id, target, reason}` and removes the proposal with
  that id at that target, listed in `result.dismissed`. Ids are unique across
  the file. A file that is not a JSON object, has unknown fields, or breaks
  these rules fails with `invalid_argument` naming the entry
  (`checks[2] (a:1): ...`); an unreadable file is `io_error`. An agent check
  on an uncovered cell covers it.
- **Re-application (2.4).** Targets carry `rowLabel`, `colLabel`, and
  `section`, the title of the workbook sheet the table is on (`재무상태표`,
  `4. 현금및현금성자산`), which anchors the table: note tables share generic
  labels, so labels alone let an inserted table capture an entry. This is
  the model's `CellRef`, also the shape of every result target, so an agent
  copies a target from a result. Labels and sections compare without
  spacing and one leading list marker (`Ⅰ.`, `1.`, `(1)`, `가.`), so a
  renumbered note still matches, but otherwise exactly (`2024년` ≠ `2025년`,
  `(원화)` ≠ `(외화)`); an unstated label or section matches anything. An
  entry applies when its labels and section are at its `T/R/C`. Otherwise
  it is `moved` with `found` when they are at exactly one other cell (in its
  section, else within three tables), and `stale` with a reason when they
  are nowhere or at several cells; both list what the stated cell holds now
  (`there`) and neither applies, so the proposal it targeted stands. A
  dismissal whose proposal is gone is stale too. Limits: two tables with the
  same labels in one section can trade places unseen, and only the target
  is verified, so a row inserted inside a term's range changes the sum
  (which shows as a failure).
- **Footing workbook (2.5).** `dsd foot FILE [--checks C] -o OUT [--force]`
  writes the `dsd xlsx` workbook plus checks, following the reference
  workbook's convention. Each check is a difference formula,
  `=ROUND(target-(SUM(a)-SUM(b)+1000*SUM(c)),6)` (the factor converts units;
  rounding to 6 places keeps binary noise in decimal sums from showing as a
  difference), formatted `General;-General;"✓"` (0 shows ✓) and red outside
  its allowance by conditional format. Every position is read through the
  cell that owns it, as the CLI reads it: a target inside a span refers to
  the merge's first cell, and a term range a span enters from outside lists
  its owning cells (`SUM(B7,C8)`). A check down a table (any check whose
  terms are not all in the target's row) sits in a check row below the
  table, in the target's column; check rows follow their target rows, one
  blank row after the table, and the content below moves down by them. A
  check across a row sits in a check column right of the table (after the
  T# marker), in the target's row. Grey labels name each check row
  (`검증 R20 자산총계 (auto)`) and column (`차이 C7 합계 (auto/agent)`). A new
  last sheet `검증` lists every check (시트, 위치, ID, 종류, 출처, 규칙, 대상,
  차이, 허용, 결과, 비고): its 차이 refers to the in-table cell, or holds the
  formula of a check between tables; 결과 is `IF(ABS(차이)<=허용,"통과","차이")`,
  and a check needing judgment has no formula, 결과 `판단 필요`, and its reason.
  총괄표 gains 검증/통과/차이/판단 필요 columns per sheet by `COUNTIFS` over
  검증's listed rows (the sheet name matched as a literal, `"=name"`), and a
  합계 row, so counts follow corrections typed into the workbook. A check on
  a table the workbook does not place counts only in 합계. Every formula carries the CLI's result, so a reader that does
  not recalculate shows the same values, and the output is byte-identical
  across runs. `result.workbook` gives the output, size, sha256, sheet
  count, and each table's range in this workbook. `dsd xlsx` output is
  unchanged. Unresolved totals, candidates, and uncovered cells are not
  rendered; they stay in the JSON for the agent.
- **Gate findings (2.7).** Comparing with the reference workbook on the
  working sample found three proposer gaps, now fixed:
  - **Percentages:** a percentage cell is a figure that never sums. A rate
    row under a column of amounts used to take the whole column out of
    every sum.
  - **Dashes over netting parts:** a lone `-` over parts that net to zero
    is a total of zero, not a total left out.
  - **Unlabelled total columns:** these need at least two holding rows
    whose total is not 0. Rows that net to zero beside a dash had made a
    coincidence look like a total.

  Movement schedules: wherever the reference workbook reports a
  difference (its formulas leave out the opening column), the matching
  checks here pass.
- **Column labels (2.3).** A unit caption heading the whole table is not
  part of a column label (it had made every label start `(단위: 원) /`).
- **Corpus (2.3).** Each proposer is measured on every private sample for
  pass, fail, unresolved, and uncovered counts; the counts are reported to the
  owner, not recorded here, since they derive from confidential documents.
  Remaining failures are rounding in tables in larger units and differences
  the documents carry; `check.py` re-evaluates every check from the exported
  workbook.

### Steps (outline)

- [x] 2.1 Settle the open decisions with the owner.
- [x] 2.2 Check model, evaluation, and the JSON result: `T/R/C` terms
  resolved through the Phase 1 placements, values read with the Phase 1
  typing (plus footnote-marked amounts, which also change the workbook and
  `check.py`'s independent parse), per-check tolerance, unit detection and
  conversion, and labels stored beside coordinates.
- [x] 2.3 Proposers, one at a time, each measured on the private corpus for
  pass / unresolved / uncovered counts: `dsd foot` with label totals,
  hierarchy (statement subtotal columns included), signed statement chains,
  movements in rows and columns, total columns, the balance identity,
  statement ↔ note, and prior closings.
- [x] 2.4 Checks file: schema, merge, validation errors, and re-application
  by coordinates verified by labels (moved and stale checks).
- [x] 2.5 Workbook rendering of checks and live counts.
- [x] 2.6 Skill reference, evals, and docs: `references/footing.md` (model,
  workflow from run to report, checks-file example), routed from `SKILL.md`
  and the Excel section of `reading.md`; eval 11 and a trigger pair.
- [x] 2.7 Gate: on the working sample, the false positives the reference
  workbook reports in movement schedules do not appear, every remaining
  difference is explained, and the owner signs off. Signed off 2026-11-23:
  no movement-schedule false positive of the reference workbook appears,
  the one remaining difference is a true difference in the document, and
  the gate report (kept with the sample, outside the repository) explains
  every other item.

## Not planned here

- Reconciling a `.dsd` with a client's Excel package.
- Writing xlsx edits back into a `.dsd`.
- Releases: version bumps, tags, and publishing follow
  [docs/release.md](docs/release.md) and are never part of this work.
