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
| Branch | `feat/xlsx-export` |
| Phase 1 | 1.1–1.7 done. Next action: step 1.8 |
| Phase 2 | Blocked on the Phase 1 gate |

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
- **Size and licenses (1.1).** The writer adds about 1.7 MB to a release
  executable (measured with the release profile on a probe; the v0.1.2
  Windows executable is 7.06 MB). New licenses are MIT, Apache-2.0, Zlib,
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
- **Corpus check (1.7).** Container cells of nested tables are compared by
  their own lines in order, since `dsd table` flattens the nested table into
  the cell. Text lines are compared as a multiset of whitespace-free lines
  against an independent ElementTree extraction that skips the document
  header, scaffolding, and tables. The check also requires byte-identical
  reruns and that every table is mapped or reported unmapped.

Open, for the owner (affects the sheet split):

- Rule 3 applies inside a notes region too, so an appropriation statement
  table (`이익잉여금처분계산서`) inside its note opens a statement sheet and
  the rest of that note follows it until the next note heading. The
  reference workbook keeps it on the note's sheet. Of the corpus, only the
  working sample has this case. Keeping it on the note would be a one-line
  rule: no statement sheets inside a notes region.

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
   the next statement title or the end of its region.
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

Kept as text: invalid grouping (`5,6,32`), leading zeros, more than 15
significant digits, an explicit `+`, a number with a footnote marker or any
other text, dates, and empty cells (left empty). Numbers are right-aligned.

Footnote-marked amounts (`1,234 (*1)`) stay text in Phase 1. Phase 2 needs
them as numbers; decide there whether to carry the marker in the format.

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
- [ ] **1.8 Docs and skill**: README command list and example,
  ARCHITECTURE boundaries, a short export section in the skill's reading
  reference with routing from `SKILL.md`. Then `/code-review` and
  `/harmonize-docs changes`.
- [ ] **1.9 Gate**: run the acceptance list below; the owner opens the
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
  "target": {"table": 13, "row": 3, "col": 4, "label": "…"},
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

### Open decisions (settle at Phase 2 start)

- Rounding tolerance for documents in thousands or millions: zero, a fixed
  ±1 per term, or stated per check.
- Unit conversion in cross checks between tables in different units.
- How far statement ↔ note matches are asserted automatically versus offered
  as candidates.
- Footnote-marked amounts as numbers (see Phase 1 cell typing).
- Whether a checks file from one draft is re-applied to the next by
  coordinates, by labels, or both; and the exact overlay schema.

### Steps (outline)

- [ ] 2.1 Settle the open decisions with the owner.
- [ ] 2.2 Check model, evaluation, and the JSON result.
- [ ] 2.3 Proposers, one at a time, each measured on the private corpus for
  pass / unresolved / uncovered counts.
- [ ] 2.4 Checks file: schema, merge, validation errors.
- [ ] 2.5 Workbook rendering of checks and live counts.
- [ ] 2.6 Skill reference, evals, and docs.
- [ ] 2.7 Gate: on the working sample, the false positives the reference
  workbook reports in movement schedules do not appear, every remaining
  difference is explained, and the owner signs off.

## Not planned here

- Reconciling a `.dsd` with a client's Excel package.
- Writing xlsx edits back into a `.dsd`.
- Releases: version bumps, tags, and publishing follow
  [docs/release.md](docs/release.md) and are never part of this work.
