# Architecture

`dsd` is one Rust implementation of `.dsd` reading, checking, and editing,
shipped as a standalone executable and driven by the `dart-dsd` agent skill.
[README.md](README.md) owns commands, installation, and updates;
[docs/release.md](docs/release.md) owns release operation.

## Boundaries

```text
skills/dart-dsd ──(dsd …; JSON on stdout)──► crates/dsd-cli ──► crates/dsd
  routing, task judgment                      process contract,   format logic
                                              release checks,
                                              upgrade
```

- [`crates/dsd`](crates/dsd/src/lib.rs) owns every `.dsd` rule: the archive
  layer ([archive.rs](crates/dsd/src/archive.rs)), the positioned XML parser
  ([xml.rs](crates/dsd/src/xml.rs)), the document model and table grids
  ([doc.rs](crates/dsd/src/doc.rs)), text extraction and verified-element
  rules ([text.rs](crates/dsd/src/text.rs)), read views
  ([views.rs](crates/dsd/src/views.rs)), checks
  ([check.rs](crates/dsd/src/check.rs)), schema validation
  ([schema.rs](crates/dsd/src/schema.rs)), edits and extract/pack
  ([edit.rs](crates/dsd/src/edit.rs)), and block diffs
  ([diff.rs](crates/dsd/src/diff.rs), [difflib.rs](crates/dsd/src/difflib.rs)).
  It has no network access and no process I/O.
- [`crates/dsd-cli`](crates/dsd-cli/src/main.rs) parses arguments (clap),
  runs one command, and writes one stdout document. It alone contacts GitHub,
  for release checks ([release_check.rs](crates/dsd-cli/src/release_check.rs))
  and `upgrade` ([upgrade.rs](crates/dsd-cli/src/upgrade.rs)).
- The skill never parses `.dsd` files itself; it chooses commands, interprets
  envelopes, and owns task judgment: dependents of a changed figure, when to
  take the text route, and what to report.

## Invariants

- **Byte preservation.** No XML is re-serialized. Edits locate their target
  in the parsed tree and splice bytes into the original `contents.xml`. The
  archive writer keeps member order, raw name bytes, flags (minus data
  descriptors), timestamps, attributes, extra fields (minus ZIP64), and
  comments, and copies unchanged members' compressed bytes.
- **Commit guard.** Every write re-checks the result against the source and
  writes nothing when it would add an error-level issue or turn CRLF into LF;
  the output is re-read and its replaced members compared before success is
  reported. Outputs never replace an existing file or the input without
  `--force`, and are written through a synced temporary file and a rename.
- **Verified scope.** Rules apply only to elements in `OBSERVED_TAGS` and
  `SCHEMA_EQUIVALENT_TAGS` ([doc.rs](crates/dsd/src/doc.rs)) and the two
  observed schemas. Anything else is shown as found, flagged `unverified-*`,
  and refused by edit commands, which point to the extract → edit → pack
  route.
- **Schema subset.** The validator implements exactly the XML Schema
  constructs the DART schemas use and reports any other as
  `schema-unsupported`; it reads the user's installed schema and ships none.
- **Bounds.** Archive size, member count, per-member and total decompressed
  size, XML nesting depth, and table grid width are capped, so hostile input
  fails with an envelope instead of exhausting memory or the stack.

## Process contract

Contract version 1 (`metadata.contract`):

- stdout carries exactly one newline-terminated document: the JSON envelope
  `{ok, command, result, warnings, error, metadata}`, or plain text for help,
  `--version`, and successful `--format text` views. Failures are always
  JSON.
- Exit 0 means `ok: true`; 1 is an execution failure with a stable
  `error.code` (`file_not_found`, `invalid_archive`, `invalid_xml`,
  `invalid_argument`, `text_not_found`, `count_mismatch`, `edit_refused`,
  `unverified_content`, `structural_error`, `output_exists`, `io_error`,
  `check_failed`, and the upgrade codes); 2 is an invalid invocation
  (`invalid_invocation`).
- Bare `dsd` prints exactly `dsd --help`.
- After a successful command, an optional top-level `advisories` array carries
  a cached release notice (a stderr line after text output). It never changes
  the result or exit status; `DSD_NO_UPDATE_CHECK=1` disables it.

## Verification

- [`crates/dsd-cli/tests/cli.rs`](crates/dsd-cli/tests/cli.rs) runs the
  executable as an external process against the synthetic fixture and its
  variants: the process contract, each command's behavior and refusals,
  byte-level edit expectations, and resource bounds.
- [`crates/dsd/tests/format_reference.rs`](crates/dsd/tests/format_reference.rs)
  keeps the verified-element lists equal to the format reference and, when
  the private schemas exist, checks the schema-equivalence claims.
- [`scripts/check.py`](scripts/check.py) runs everything above plus a CLI
  smoke run over the private corpus that compares `read` with an independent
  ElementTree extraction and requires byte-identical extract/pack.
- The Rust implementation replaced a Python helper. Before cutover a parity
  harness matched its text views, check results, edit outputs, and diffs on
  the public fixture and the private corpus; both live in the history before
  the cutover commit.
- Release artifacts are built and certified natively on every target; see
  [docs/release.md](docs/release.md).
