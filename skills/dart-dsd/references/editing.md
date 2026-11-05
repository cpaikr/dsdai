# Edit a document

Write every edit to a new file such as `<name>_수정.dsd`. Overwrite the
original only when the user explicitly authorizes it (`--force`).

Two routes keep the container intact, and both end in the same verification:

- **Edit commands** (`set-cell`, `replace`, `copy-row`, `delete-row`,
  `apply`) locate and splice the change. They refuse (`edit_refused`,
  `unverified_content`, `count_mismatch`) when the edit's meaning is ambiguous
  or depends on something unverified: a table that contains or sits inside an
  unverified element, a `replace` hit inside one, or an unverified schema.
- **Text editing**: `dsd extract FILE DIR`, edit `DIR/contents.xml` as text,
  then `dsd pack DIR FILE -o OUT`. Use it when a command refuses or the change
  is outside the commands' scope (new paragraphs or sections, column or span
  changes, unverified content). Read the surrounding XML first and follow the
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
   - `copy-row` (optionally `--clear`, `--count`) and `delete-row` honor the
     form's row locks (`ACOPY`/`ADELETE="N"`) and refuse rows inside a
     `ROWSPAN`. Override locks with `--force-locked` only when the user wants
     to depart from the form.
   - `apply FILE EDITS.json -o OUT` runs a JSON array of those operations in
     order and writes one file; `dsd apply --help` lists each operation's
     fields (e.g. `{"op": "set-cell", "table": 3, "row": 4, "col": 2,
     "text": "1,100,000"}`). Later operations see earlier results, so list row
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
3. Report the output path, the changes made, dependents left unchanged, the
   checks run (with schema validation stated as done or skipped), anything
   refused or deferred, and any skill problem worth reporting. Opening the
   file in DART Editor and running its own validation before creating a
   `.DRT` is the user's final check.
