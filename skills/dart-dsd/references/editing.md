# Edit a document

Write every edit to a new file such as `<name>_수정.dsd`. Overwrite the
original only when the user explicitly authorizes it (`--force`).

Two routes keep the container intact, and both end in the same verification:

- **Edit commands** (`set-cell`, `replace`, the structural commands below,
  and `apply`) locate and splice the change. They refuse (`edit_refused`,
  `unverified_content`, `count_mismatch`) when the edit's meaning is ambiguous
  or depends on something unverified: a table that contains or sits inside an
  unverified element, a `replace` hit inside one, or an unverified schema.
- **Text editing**: `dsd extract FILE DIR`, edit `DIR/contents.xml` as text,
  then `dsd pack DIR FILE -o OUT`. Use it when a command refuses or the change
  is outside the commands' scope (new titled sections, merging or splitting
  cells, unverified content). Read the surrounding XML first and follow the
  [format reference](format.md)'s text conventions and size and TOC rules:
  UTF-8 without BOM, CRLF kept, `&cr;` for line breaks. `pack` restores the
  original archive layout and refuses structural damage, a BOM, and CRLF to
  LF conversion.

## Steps

1. **Locate.** Use `find` and `table` to get `T#`/`R#`/`C#` and confirm the
   cell holds what the user means. The same figure often appears in several
   statements, notes, and periods; identify each intended occurrence. Map
   periods from the header row: 당기 is the column headed with the larger
   `제 N 기` (usually the first value column), 전기 the smaller.
2. **Choose the operation.**
   - `set-cell` replaces one cell's text, keeping its `P`/`SPAN` wrapper and
     attributes; newlines become `&cr;`:
     `dsd set-cell FILE -o OUT --table 3 --row 4 --col 2 --text 1,100,000`. A `TU` cell with an `AUNITVALUE`
     needs `--unit-value` in machine form: dates `YYYYMMDD`; currency units
     (`AUNIT=WON`) as codes `1`=원, `2`=천원, `3`=백만원. A value that
     disagrees with the text is refused. Cells with mixed or multi-paragraph
     content are refused; hand-edit them, or use `--flatten` only when losing
     that formatting is acceptable. A position covered by another cell's span
     is refused; the hint names the spanning cell.
   - `replace --old A --new B` changes document text within a single text
     run. It never matches tags, attribute values, comments, entity
     references, or scaffolding, and cannot match across `SPAN`/`P`
     boundaries. It fails unless the occurrence count equals `--expect`
     (default 1). Run `--dry-run` first: its `count`, not `find`'s
     whitespace-insensitive one, is what `--expect` must equal; each
     occurrence shows context, any `unitValue` it will not change, and
     `unverified` containers. Scope with `--table N`. Hits inside `TU` cells
     change only the display text; follow up with `set-cell --unit-value`.
     `--except PHRASE` (repeatable) leaves hits inside that phrase alone, so
     `--old 반기 --new 분기 --except 분ㆍ반기 --expect any` keeps the name of
     the review standard; the dry run reports them as `skipped`.
   - Structure: `copy-row`/`delete-row`, `copy-col`/`delete-col`
     (`--table N --col C`), `copy-table`/`delete-table` (`--table N`), and
     `insert-paragraph` (`--after-table`, `--before-table`, or `--after-line`
     of a free paragraph) / `delete-paragraph --line L`. Copies take
     `--clear` and `--count`. A copied row or column joins the spans it lies
     inside (the group cell grows); a deletion shrinks them, and a spanning
     cell moves down when its own row goes. `HEIGHT`, `WIDTH`, and the table
     and `COLGROUP` widths are recomputed. Table and line numbers after a
     table or paragraph change shift; re-read them. The commands honor form
     locks (`ACOPY`/`ADELETE`, `ACOPYCOL`/`ADELETECOL`, tables in a
     `TABLE-GROUP`); override with `--force-locked` only when the user wants
     to depart from the form. A paragraph anchored on a `TABLE-GROUP` table
     goes before or after the whole group.
   - `apply FILE EDITS.json -o OUT` runs a JSON array of those operations in
     order and writes one file; `dsd apply --help` lists each operation's
     fields (e.g. `{"op": "set-cell", "table": 3, "row": 4, "col": 2,
     "text": "1,100,000"}`). Later operations see earlier results, so list
     insertions and deletions bottom-up or account for the shift.
   - For anything else, or when a command refuses, use the text-editing
     route. Do not reach for `--flatten` or `--force-locked` just to get past
     a refusal.
3. **Match the form's conventions.** Format values like their neighbors:
   thousands separators, `(1,234)` negatives, `-` for nil, leading-space
   indentation, and spacing in titles. A value the user spelled out exactly
   is written as given; mention any convention it departs from. Keep `ACODE`,
   `AUNIT`, `USERMARK`, sizes, and `DOCUMENT-HEADER`/`meta.xml` values unless
   the request concerns them. Values may start with `-` (`--text -1,000`).
4. **Carry related figures.** The tool does not recompute totals or
   cross-references. A changed figure can affect subtotals, totals, other
   statements, and notes. A changed period can affect the other period bound,
   its repeated `PERIODFROM2`/`PERIODTO2` copy, `제 N 기` labels, and dates in
   paragraphs. Update those too when the request covers them; otherwise list
   them for the user as unchanged dependents.

Every edit re-checks its result and writes nothing if the change would add a
structural error or turn CRLF line endings into LF (`structural_error`, with
`error.issues`). A successful edit reports `result.output`, `sha256`, and
`changes`; new warnings (such as `period-mismatch`) and notes appear in
`warnings`.

## Roll a document forward

Updating a report to the next period (반기 → 3분기, a year-end) changes the
period, the wording, and most figures at once.

1. **Settle the deliverable first.** Ask which form the result must be when it
   is not stated: the reviewed report (분기검토보고서) or the company's
   statements for submission (감사전 재무제표) differ in cover, review report,
   titles, and form code. Do not switch `DOCUMENT-NAME ACODE` or `meta.xml`
   IDs on your own; report the switch for the user or DART Editor to make.
2. **Periods.** Update the cover and statement period cells with
   `set-cell --unit-value` (`PERIODFROM`/`PERIODTO` and their `…2` copies),
   then wording with ordered `replace --expect any` edits in one `apply`,
   using `except` for phrases that must stay (`분ㆍ반기`). Dry-run each first.
3. **Figures.** Take each table from the source package. For comparatives
   the package lacks, use the filed report for that period on DART and say
   so. Check that tables foot before writing.
4. **Structure.** Remove tables the new period does not need, add rows and
   columns for new items, and check whether the preparer keeps columns for
   items that have ended (a fully redeemed bond) or drops them.
5. **Compare.** When the user has a reference version (last period's filed
   report, a colleague's draft), run `dsd compare REFERENCE OUT --format html
   -o compare.html` and review the differences with the user rather than
   asserting either side is right.

## Verify and report

1. Run `dsd check OUT --against ORIGINAL`. It checks structure, image and
   `meta.xml` consistency, TOC numbering, table grids, size conventions, `TU`
   date and currency-unit values, repeated period copies, and line endings,
   and validates against the DART schema when a schema directory is
   available (`--schema-dir`, `DART_SCHEMA_DIR`, or `C:\DART\Resource\Dtd`);
   otherwise it reports `schema-skipped`, and `schema-unsupported` when the
   installed schema uses constructs the validator does not implement.
2. Run `dsd diff ORIGINAL OUT` and confirm every change was intended. If the
   raw line count exceeds what the block list explains, inspect with `--raw`.
   When the edit restructured the document, `dsd compare ORIGINAL OUT
   --format text` shows the changes table by table instead.
3. Report the output path, the changes made, dependents left unchanged, the
   checks run (with schema validation stated as done or skipped), anything
   refused or deferred, and any skill problem worth reporting. Opening the
   file in DART Editor and running its own validation before creating a
   `.DRT` is the user's final check.
