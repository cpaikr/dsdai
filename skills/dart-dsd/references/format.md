# DSD Format Reference

Lookup material for interpreting `.dsd` content, editing `contents.xml` by
hand, and diagnosing `check` warnings. FSS publishes no container
specification; everything here was observed in DART Editor output (Dart3 and
Dart4 documents, editor versions 3.x–5.1x) and is labeled **inferred** where
the meaning is a reasoned guess rather than an observed invariant.

## Container

- A `.dsd` is an unencrypted ZIP (Deflate) with `contents.xml` first,
  `meta.xml` second, then any images. Member order, the DOS timestamp, and
  version fields are DART Editor's; keep them.
- Image member names are usually Korean and stored as **CP949 bytes without
  the ZIP UTF-8 flag**. Generic tools show them as mojibake, and re-zipping
  with a modern tool rewrites them as flagged UTF-8, which breaks the match
  with the names referenced inside the XML.
- A `.dsd` is the editable working file. The signed submission package is a
  separate `.DRT` produced by DART Editor; this skill never creates one.

## `meta.xml`

Small UTF-8 sidecar:

| Element | Holds |
|---|---|
| `GENERATOR` | `schema` (`dart3.xsd`/`dart4.xsd`) and `editver` (editor version) |
| `DOCUMENT-HEADER` | `regcik` (corporate code) and `regname`, often empty |
| `DOCUMENT-INFO` | `docver` (form version), `bsn-id`, `rpt-id`, `doc-id` (form codes), `iscorrection` (`Y` for a correction filing) |
| `FILE-INFO` | submission bookkeeping, normally empty |
| `DOCUMENT-IMG-LIST/IMG` | one `filename` per embedded image |

Leave `meta.xml` alone unless images change.

## `contents.xml` text conventions

- UTF-8 without BOM, `<?xml version="1.0" encoding="utf-8"?>`, CRLF line
  endings, no tabs, no comments, no CDATA, no DOCTYPE.
- Element and attribute names are uppercase. Attribute order varies between
  editor versions; it is not significant, but keep the existing order when
  editing so diffs stay small.
- Empty elements are written as start/end pairs (`<COL WIDTH="300"></COL>`),
  never `<COL/>`.
- **Line breaks inside text are the literal token `&cr;`**, which appears in
  XML as `&amp;cr;`. Literal newlines in the XML are layout only. Several
  `&cr;` in an otherwise empty `<P>` produce vertical space.
- Text escapes `&`, `<`, `>`, and `"` (`&quot;`); apostrophes and other
  characters, including `’`, `※`, `◆`, `Ⅰ`, appear literally.
- Spaces are significant: leading spaces indent account names in statements,
  and runs of spaces space out titles (`감   사   보   고   서`). An
  ideographic space `U+3000` often fills a cell that should look empty.

## Document skeleton

```text
DOCUMENT  (xsi:noNamespaceSchemaLocation="dart4.xsd" or "dart3.xsd")
├── DOCUMENT-HEADER   DOCUMENT-NAME (ACODE form code), FORMULA-VERSION (ADATE),
│                     COMPANY-NAME (AREGCIK), SUMMARY/EXTRACTION
└── BODY (ATOCID)
    ├── COVER, TOC          cover page and table of contents
    ├── INSERTION           a form slot; holds COMMENT, LIBRARYLIST, and,
    │   └── LIBRARY         once filled, FILENAME plus the inserted content
    │       └── SECTION-1 … ordinary content
    ├── PART, SECTION-1 … SECTION-7   nested sections, each starting with TITLE
    ├── TABLE-GROUP (ACLASS form code) / TABLE
    ├── P, IMAGE, PGBRK, WARNING, APPENDIX
```

- Section depth is the number in `SECTION-n`, not XML depth: a `SECTION-1`
  can sit inside a `LIBRARY` that is itself inside a `SECTION-2`.
- `COMMENT`, `LIBRARYLIST`, `FILENAME`, `WARNING`, and `APPENDIX` comments are
  editor guidance (`◆click◆ …`) that DART does not print. An `INSERTION`
  without a `LIBRARY` is an unused optional slot.
- `DOCUMENT-HEADER/SUMMARY/EXTRACTION` values (`ACODE` such as `FIN_STAT`,
  `TOT_ASSETS`) feed DART's data extraction. Change them only on request.

## Inline and block elements

| Element | Notes |
|---|---|
| `P` | paragraph; optional `USERMARK` formatting; mixed text and `SPAN` |
| `SPAN` | inline run with its own `USERMARK` |
| `A` | cross-reference; `REFNO` holds section, table, and appendix identifiers joined by vertical bars |
| `TITLE` | section title; `ATOC="Y"` puts it in the TOC; `ATOCID` is its TOC number; `AASSOCNOTE` links note structures; `ENG` English label |
| `COVER-TITLE` | report title on the cover |
| `IMAGE` | wraps `IMG` (text = archive member name; `WIDTH`, `HEIGHT`, `ALIGN`, `HSPACE`) and `IMG-CAPTION` |
| `PGBRK` | page break, always empty |

`USERMARK` tokens (**inferred**): `F-n` font size n, `F-BTn`/`F-GLn` a font
family at size n, `B` bold, `!B` not bold, `0X` plus six hex digits a text
color, `BC0X` plus six hex digits a background color, `A-L` left alignment.
Copy them from neighbors rather than composing new ones.

## Tables

```text
TABLE (ACLASS NORMAL|EXTRACTION|FINANCE, WIDTH, BORDER 0|1, AFIXTABLE Y|N)
├── COLGROUP (WIDTH) / COL (WIDTH) …
├── THEAD / TR / TH …
└── TBODY / TR / TD | TE | TU …
```

- `TR` carries `ACOPY` and `ADELETE`: `N` means the form forbids copying or
  deleting that row in DART Editor. `TH` carries `ACOPYCOL`, `ADELETECOL`,
  `AMOVECOL` for columns. `AFIXTABLE="Y"` marks a form-fixed table.
- `TD` is an ordinary cell. `TE` is an extraction cell whose `ACODE` names the
  data item DART collects. `TU` is a unit cell: its text is the display form
  and `AUNITVALUE` the machine value (dates as `YYYYMMDD`; codes such as
  `PB_TYPE=1`), with `AUNIT` naming the kind. Keep `AUNITVALUE` in step with
  the text; it is empty in some forms.
- The currency-unit cell (`AUNIT="WON"`, text like `(단위 : 천원)`) stores a
  code, not a multiplier: `1` = 원, `2` = 천원, `3` = 백만원. No other code
  has been observed. `set-cell` refuses, and `check` warns with
  `unit-value`, when the code is unknown or disagrees with the unit the text
  names.
- Forms that state the reporting period twice (cover and the attached
  statements' title) write the second copy as `PERIODFROM2`/`PERIODTO2`; in
  every observed file it holds the same dates as `PERIODFROM`/`PERIODTO`.
  Change both copies together; `check` warns with `period-mismatch` when they
  disagree.
- `AUPDATECONT="N"` (**inferred**) marks content the form does not expect the
  user to change.
- `ROWSPAN`/`COLSPAN` follow HTML rules. Every row must cover every declared
  column exactly once after spans are expanded; `check` reports a
  `table-grid` error otherwise.
- Numbers are text, formatted by the preparer: thousands separators,
  negatives in parentheses `(1,234)` (a leading `-` is rare), `-` for nil, and
  percentages with `%`. Follow the column's existing style.

### Size arithmetic

These invariants hold across the observed corpus; keep them when changing
structure by hand.

- Cell `HEIGHT` = sum of the spanned rows' `TR HEIGHT` − 7.
- Dart4-era editors: `TABLE WIDTH` = `COLGROUP WIDTH` = sum of `COL WIDTH`;
  cell `WIDTH` = sum of spanned `COL WIDTH` − 9.
- One Dart3-era file instead used `TABLE WIDTH` = sum of `COL WIDTH` + 9 ×
  column count, and cell `WIDTH` = sum of spanned `COL WIDTH` + 9 × `COLSPAN`.
  `check` detects each table's convention and warns about cells that break it.

## TOC numbering

`ATOCID` values on `TITLE`/`COVER-TITLE` are unique integers, and
`BODY ATOCID` equals the largest of them. When adding a titled section by hand,
give its `TITLE` the next number and raise `BODY ATOCID` to match; renumbering
existing IDs is unnecessary.

## Images

An image appears in three places that must agree byte for byte after
decoding: the archive member name, the `IMG` text in `contents.xml`, and an
`IMG filename` entry in `meta.xml`. Prefer inserting or replacing images in
DART Editor. When an image must be replaced programmatically with the same
name, replace only the member bytes through `extract`/`pack`.

## Verified coverage

The CLI's rules were checked against real Dart3 and Dart4 DART Editor
output and the synthetic test fixture. These elements occur there and are
treated as verified:

`A` `APPENDIX` `BODY` `COL` `COLGROUP` `COMMENT` `COMPANY-NAME` `COVER`
`COVER-TITLE` `DOCUMENT` `DOCUMENT-HEADER` `DOCUMENT-NAME` `EXTRACTION`
`FILENAME` `FORMULA-VERSION` `IMAGE` `IMG` `IMG-CAPTION` `INSERTION`
`LIBRARY` `LIBRARYLIST` `P` `PART` `PGBRK` `SECTION-1` `SECTION-2`
`SECTION-3` `SPAN` `SUMMARY` `TABLE` `TABLE-GROUP` `TBODY` `TD` `TE` `TH`
`THEAD` `TITLE` `TOC` `TR` `TU` `WARNING`

Verified by schema equivalence: these elements have not been observed, but
both DART schemas give them the same content model and attributes as an
observed element that the CLI handles by the same rule:

`SECTION-4` `SECTION-5` `SECTION-6` `SECTION-7` `TFOOT`

`SECTION-4` to `SECTION-6` match `SECTION-2`, and `SECTION-7` matches it
except that it allows no deeper section. `TFOOT` matches `TBODY`. The
CLI's tests re-check this against the installed schemas. **Inferred:**
DART Editor's `ATOCID` numbering in sections this deep is unobserved, so add
deep sections through the text route and keep `check`'s TOC warnings clean.

The DART schemas also allow elements that remain unverified: `CAUTION`,
`CORRECTION`, `DESCRIPTION`, `EMPHASIS`, `NOTE`, `NOTE-GROUP`, `SUBTITLE`,
and a lowercase `section-5`. Their layout conventions and any coupling
between their text and attributes are unknown. Documents from before
`dart3.xsd` (the older DTD formats) are also unverified.

When a document contains one of these, or declares another schema:

- Treat the CLI's rendering of it as text only, and read the raw XML at
  the reported line before relying on its meaning.
- Edit it with `extract` → edit `contents.xml` → `pack`. Copy the pattern of
  a neighboring instance of the same element when one exists rather than
  inventing structure.
- Schema validation in `check` knows every allowed element, so it is the
  main structural safeguard here. State whether it ran, and recommend
  checking the result in DART Editor.

Verifying a new element means testing `read`, `table`, and the edit commands
on a real document that uses it, then adding it to `OBSERVED_TAGS` in
the CLI's `crates/dsd/src/doc.rs` with a test. Schema equivalence (`SCHEMA_EQUIVALENT_TAGS`)
applies only when the content model and attributes match an observed
element exactly and the CLI already handles both with the same code.

## Manual edit recipes

The structural commands (`copy-row`, `copy-col`, `copy-table`,
`insert-paragraph`, and their deletions) cover rows and columns with spans,
whole tables, and plain paragraphs; use these recipes with `extract` → edit
`contents.xml` → `pack` only for what they do not:

- **A paragraph with attributes:** copy a neighboring `<P …>…</P>` line,
  including its `USERMARK`, and change the text using the escapes above.
- **Merge or split cells:** set `ROWSPAN`/`COLSPAN` on the remaining cell,
  remove the covered cells, and recompute `HEIGHT`/`WIDTH` with the size
  arithmetic.
- **Add a titled section:** copy an existing `SECTION-n` shell with the same
  `ACLASS`, give its `TITLE` a new `ATOCID`, and update `BODY ATOCID`.
