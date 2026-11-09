//! The command model. Help text here is the CLI's only command inventory.

use std::path::PathBuf;

use clap::{Args, Parser, Subcommand, ValueEnum};

const ROOT_AFTER: &str = "\
Locators:
  Tables are numbered T1.. in document order; rows R1.. count THEAD, TBODY,
  and TFOOT rows; columns C1.. are grid columns after COLSPAN/ROWSPAN expansion.

Typical flow:
  1. dsd inspect report.dsd                  archive, schema, counts (no text)
  2. dsd outline report.dsd --format text    sections, tables, images
  3. dsd find report.dsd 매출액               locate a figure or phrase
  4. dsd table report.dsd 3 --format text    cells with R#/C# coordinates
  5. dsd set-cell report.dsd -o out.dsd --table 3 --row 4 --col 2 --text 1,100
  6. dsd check out.dsd --against report.dsd && dsd diff report.dsd out.dsd
  7. dsd compare filed.dsd draft.dsd --format html -o compare.html

Output:
  Commands print one JSON object to stdout: {ok, command, result, warnings,
  error, metadata}. Exit status 0 = ok, 1 = failure (including `check` errors),
  2 = invalid invocation. Views accept --format text for a compact listing;
  failures stay JSON. --pretty indents JSON. Help and --version are plain text.
  Edits always write a new file and refuse to replace one without --force.

Updates:
  After a successful command, dsd may report a newer release: one entry in the
  JSON `advisories` array, or a stderr line after --format text output. The
  result and exit status never change. DSD_NO_UPDATE_CHECK=1 disables this; see
  `dsd version --help` and `dsd upgrade --help`.";

#[derive(Debug, Parser)]
#[command(
    name = "dsd",
    bin_name = "dsd",
    version,
    about = "Read, check, and surgically edit DART Editor .dsd documents.",
    after_help = ROOT_AFTER,
    disable_help_subcommand = false,
    propagate_version = false
)]
pub struct Cli {
    /// Indent JSON output.
    #[arg(long, global = true)]
    pub pretty: bool,

    #[command(subcommand)]
    pub command: Command,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, ValueEnum)]
pub enum Format {
    Json,
    Text,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, ValueEnum)]
pub enum CompareFormat {
    Json,
    Text,
    Html,
}

#[derive(Debug, Args)]
pub struct FormatArg {
    /// Output format: one JSON envelope, or a compact text listing.
    #[arg(long, value_enum, default_value_t = Format::Json)]
    pub format: Format,
}

#[derive(Debug, Args)]
pub struct WriteArgs {
    /// New .dsd path; never the input unless --force.
    #[arg(short, long, value_name = "OUT")]
    pub output: PathBuf,
    /// Replace an existing output file.
    #[arg(long)]
    pub force: bool,
}

#[derive(Debug, Args)]
pub struct CellTarget {
    /// Table number (T#).
    #[arg(long, value_name = "N")]
    pub table: usize,
    /// Row number (R#).
    #[arg(long, value_name = "N")]
    pub row: usize,
}

#[derive(Debug, Args)]
pub struct ColTarget {
    /// Table number (T#).
    #[arg(long, value_name = "N")]
    pub table: usize,
    /// Grid column number (C#).
    #[arg(long, value_name = "N")]
    pub col: usize,
}

const COMPARE_AFTER: &str = "\
Pairs tables and paragraphs by content, not position, so documents built on
different forms, or with tables inserted, removed, or reordered, still line up.
Within a table pair, rows are matched by their label (footnote markers like (*1)
and 주식회사/(주)/㈜ spellings are ignored) and columns by their headers when the
column counts differ. Numbers compare as numbers: kind \"value\" carries
delta = OTHER - BASE; \"format\" is the same number written differently.

JSON and text list only items that differ unless --all; html renders each
document in its own pane at DART's sizes, aligned item by item and scrolling
together, with changed cells and words highlighted, a change map, a section
index, next/previous navigation (n/p), a changes-only filter, and a panel
listing the selected item's changes (i). The report is one self-contained file
with no external resources; it needs -o.";

const STRUCTURE_AFTER: &str = "\
Copies join every span they lie inside: copying a row that sits under a
ROWSPAN grows that cell (ROWSPAN and HEIGHT) instead of duplicating it, and
copying a column crossed by a COLSPAN widens that cell. Deleting shrinks spans;
a cell that spans down from a deleted row moves to the next row. Sizes follow
DART Editor's arithmetic: cell HEIGHT = spanned TR HEIGHT - 7, cell and table
WIDTH from the COL widths in the table's own convention.";

#[derive(Debug, Subcommand)]
pub enum Command {
    /// Archive, metadata, structure counts, and issues; no document text.
    Inspect { file: PathBuf },
    /// Sections, tables (T#), insertions, and images with contents.xml line numbers.
    Outline {
        file: PathBuf,
        /// Also list paragraphs, warnings, and empty insertions.
        #[arg(long)]
        all: bool,
        #[command(flatten)]
        format: FormatArg,
    },
    /// Every block with its line, element path, and exact text; tables as T# stubs.
    #[command(after_help = "\
Text is exact: nothing is trimmed or collapsed. JSON gives DART's &cr; line
break as \\n; --format text quotes each block as a JSON string with ⏎ for &cr;.
Elements inside text other than SPAN/A stay visible as ⟦TAG⟧…⟦/TAG⟧, and
elements without a rule appear with their attributes, never hidden. Read table
cells with `dsd table`; images stay in the archive (see `dsd extract`).")]
    Read {
        file: PathBuf,
        /// Write the view to this file instead of stdout.
        #[arg(short, long, value_name = "PATH")]
        output: Option<PathBuf>,
        /// Replace an existing --output file.
        #[arg(long)]
        force: bool,
        /// Also show editor scaffolding (COMMENT, LIBRARYLIST, FILENAME).
        #[arg(long)]
        scaffold: bool,
        /// Omit the leading # metadata lines of --format text.
        #[arg(long)]
        no_header: bool,
        #[command(flatten)]
        format: FormatArg,
    },
    /// Table cells with R#/C# coordinates, spans, row locks, and unit metadata.
    Table {
        file: PathBuf,
        /// Table numbers (T#).
        #[arg(required = true, value_name = "N")]
        tables: Vec<usize>,
        /// List every grid position covered by a span.
        #[arg(long)]
        spans: bool,
        #[command(flatten)]
        format: FormatArg,
    },
    /// Locate text in cells and paragraphs, ignoring whitespace unless --exact.
    Find {
        file: PathBuf,
        /// Text to find; may start with '-'.
        #[arg(allow_hyphen_values = true)]
        text: String,
        /// Match whitespace exactly.
        #[arg(long)]
        exact: bool,
        /// Stop after this many matches.
        #[arg(long, default_value_t = 50, value_parser = clap::value_parser!(u32).range(1..))]
        limit: u32,
        #[command(flatten)]
        format: FormatArg,
    },
    /// Structural checks, optional DART schema validation, and comparison with an original.
    #[command(after_help = "\
Exit status 1 with error.code \"check_failed\" when an error-level issue is
found; the issues are still in result. Schema validation uses a locally
installed DART schema directory: --schema-dir, else DART_SCHEMA_DIR, else
C:\\DART\\Resource\\Dtd when present; without one it reports schema-skipped.")]
    Check {
        file: PathBuf,
        /// Compare members and structure with the original .dsd.
        #[arg(long, value_name = "ORIGINAL")]
        against: Option<PathBuf>,
        /// Directory holding Dart3.xsd/Dart4.xsd.
        #[arg(long, value_name = "DIR")]
        schema_dir: Option<PathBuf>,
        /// Skip schema validation.
        #[arg(long)]
        no_schema: bool,
    },
    /// Align two documents by content and report table, row, cell, and wording differences.
    #[command(after_help = COMPARE_AFTER)]
    Compare {
        /// The reference document (left side; deltas are OTHER - BASE).
        base: PathBuf,
        /// The document to compare with it (right side).
        other: PathBuf,
        /// Output: one JSON envelope, a compact text listing, or an HTML report (needs -o).
        #[arg(long, value_enum, default_value_t = CompareFormat::Json)]
        format: CompareFormat,
        /// Write the comparison to this file; stdout then carries a summary envelope.
        #[arg(short, long, value_name = "PATH")]
        output: Option<PathBuf>,
        /// Replace an existing --output file.
        #[arg(long)]
        force: bool,
        /// Also list items that are the same (JSON and text).
        #[arg(long)]
        all: bool,
        /// List at most this many items (JSON and text).
        #[arg(long, default_value_t = 500)]
        limit: usize,
    },
    /// Changed cells and text blocks between two .dsd files, then raw line and member changes.
    Diff {
        original: PathBuf,
        edited: PathBuf,
        /// Show at most this many block changes.
        #[arg(long, default_value_t = 200)]
        limit: usize,
        /// Also list the changed contents.xml lines.
        #[arg(long)]
        raw: bool,
        #[command(flatten)]
        format: FormatArg,
    },
    /// Replace one table cell's text, keeping its P/SPAN wrapper and attributes.
    #[command(after_help = "\
A TU cell with an AUNITVALUE needs --unit-value in machine form: dates as
YYYYMMDD; currency units (AUNIT=WON) as codes 1=원, 2=천원, 3=백만원. A value
that disagrees with --text is refused. Newlines in --text become DART line
breaks. Values may start with '-' (--text -1,000).")]
    SetCell {
        file: PathBuf,
        #[command(flatten)]
        write: WriteArgs,
        #[command(flatten)]
        target: CellTarget,
        /// Column number (C#).
        #[arg(long, value_name = "N")]
        col: usize,
        /// New cell text.
        #[arg(long, allow_hyphen_values = true)]
        text: String,
        /// New AUNITVALUE for a TU cell that carries one.
        #[arg(long, allow_hyphen_values = true, value_name = "VALUE")]
        unit_value: Option<String>,
        /// Replace mixed or multi-paragraph content with plain text.
        #[arg(long)]
        flatten: bool,
    },
    /// Replace literal document text within single text runs, guarded by an expected count.
    #[command(after_help = "\
Never matches tags, attribute values, comments, entity references, or editor
scaffolding, and cannot match across SPAN/P boundaries. A newline in
--old/--new means a DART line break. Run --dry-run first: its count, not
find's whitespace-insensitive one, is what --expect must equal.")]
    Replace {
        file: PathBuf,
        /// New .dsd path; required unless --dry-run.
        #[arg(short, long, value_name = "OUT", required_unless_present = "dry_run")]
        output: Option<PathBuf>,
        /// Replace an existing output file.
        #[arg(long)]
        force: bool,
        /// List the occurrences with context and write nothing.
        #[arg(long)]
        dry_run: bool,
        #[arg(long, allow_hyphen_values = true)]
        old: String,
        #[arg(long, allow_hyphen_values = true)]
        new: String,
        /// Required number of occurrences, or `any`.
        #[arg(long, default_value = "1")]
        expect: String,
        /// Limit to table N.
        #[arg(long, value_name = "N")]
        table: Option<usize>,
        /// Leave occurrences inside this phrase alone (repeatable): --old 반기 --except 분ㆍ반기.
        #[arg(long, allow_hyphen_values = true, value_name = "PHRASE")]
        except: Vec<String>,
    },
    /// Duplicate a table row below itself; copies join the ROWSPANs the row lies in.
    #[command(after_help = STRUCTURE_AFTER)]
    CopyRow {
        file: PathBuf,
        #[command(flatten)]
        write: WriteArgs,
        #[command(flatten)]
        target: CellTarget,
        /// Number of copies (1-1000).
        #[arg(long, default_value_t = 1, value_parser = clap::value_parser!(u32).range(1..))]
        count: u32,
        /// Empty the copied cells' text (and TU AUNITVALUE).
        #[arg(long)]
        clear: bool,
        /// Ignore ACOPY="N" (form-locked rows).
        #[arg(long)]
        force_locked: bool,
    },
    /// Delete a table row; spans crossing it shrink.
    #[command(after_help = STRUCTURE_AFTER)]
    DeleteRow {
        file: PathBuf,
        #[command(flatten)]
        write: WriteArgs,
        #[command(flatten)]
        target: CellTarget,
        /// Ignore ADELETE="N" (form-locked rows).
        #[arg(long)]
        force_locked: bool,
    },
    /// Duplicate a grid column to the right of itself; COL widths and spans follow.
    #[command(after_help = STRUCTURE_AFTER)]
    CopyCol {
        file: PathBuf,
        #[command(flatten)]
        write: WriteArgs,
        #[command(flatten)]
        target: ColTarget,
        /// Number of copies (1-1000).
        #[arg(long, default_value_t = 1, value_parser = clap::value_parser!(u32).range(1..))]
        count: u32,
        /// Empty the copied cells' text (and TU AUNITVALUE).
        #[arg(long)]
        clear: bool,
        /// Ignore ACOPYCOL="N" (form-locked columns).
        #[arg(long)]
        force_locked: bool,
    },
    /// Delete a grid column; COL widths and spans follow.
    #[command(after_help = STRUCTURE_AFTER)]
    DeleteCol {
        file: PathBuf,
        #[command(flatten)]
        write: WriteArgs,
        #[command(flatten)]
        target: ColTarget,
        /// Ignore ADELETECOL="N" (form-locked columns).
        #[arg(long)]
        force_locked: bool,
    },
    /// Duplicate a table right after itself; later tables are renumbered.
    CopyTable {
        file: PathBuf,
        #[command(flatten)]
        write: WriteArgs,
        /// Table number (T#).
        #[arg(long, value_name = "N")]
        table: usize,
        /// Number of copies (1-1000).
        #[arg(long, default_value_t = 1, value_parser = clap::value_parser!(u32).range(1..))]
        count: u32,
        /// Empty every cell of the copies (and TU AUNITVALUE).
        #[arg(long)]
        clear: bool,
        /// Allow a table inside a form-managed TABLE-GROUP (cover and period blocks).
        #[arg(long)]
        force_locked: bool,
    },
    /// Remove a table; later tables are renumbered.
    DeleteTable {
        file: PathBuf,
        #[command(flatten)]
        write: WriteArgs,
        /// Table number (T#).
        #[arg(long, value_name = "N")]
        table: usize,
        /// Allow a table inside a form-managed TABLE-GROUP (cover and period blocks).
        #[arg(long)]
        force_locked: bool,
    },
    /// Insert a plain paragraph after or before a table, or after a paragraph.
    #[command(after_help = "\
Give exactly one anchor. --after-line takes the contents.xml line of a free
paragraph (one not inside a table cell) from `dsd read`. The paragraph is
written as <P>text</P>; newlines in --text become DART line breaks.")]
    #[command(group(clap::ArgGroup::new("anchor").required(true).args(["after_table", "before_table", "after_line"])))]
    InsertParagraph {
        file: PathBuf,
        #[command(flatten)]
        write: WriteArgs,
        /// Paragraph text; may start with '-'.
        #[arg(long, allow_hyphen_values = true)]
        text: String,
        /// Insert after table N.
        #[arg(long, value_name = "N")]
        after_table: Option<usize>,
        /// Insert before table N.
        #[arg(long, value_name = "N")]
        before_table: Option<usize>,
        /// Insert after the paragraph that starts on this line.
        #[arg(long, value_name = "LINE")]
        after_line: Option<usize>,
    },
    /// Remove the free paragraph that starts on a contents.xml line.
    DeleteParagraph {
        file: PathBuf,
        #[command(flatten)]
        write: WriteArgs,
        /// Line of the paragraph, from `dsd read`.
        #[arg(long, value_name = "LINE")]
        line: usize,
    },
    /// Apply a JSON array of edits in order and write one new .dsd.
    #[command(after_help = crate::help::APPLY_AFTER)]
    Apply {
        file: PathBuf,
        /// JSON file: [{"op": "set-cell"|"replace"|"copy-row"|..., ...}]; see below.
        edits: PathBuf,
        #[command(flatten)]
        write: WriteArgs,
    },
    /// Unpack members for hand-editing contents.xml, with a repack manifest.
    Extract {
        file: PathBuf,
        directory: PathBuf,
        /// Allow a non-empty directory.
        #[arg(long)]
        force: bool,
    },
    /// Repack an extracted directory with the original archive's layout.
    #[command(after_help = "\
Member order, raw CP949 names, flags, and timestamps come from ORIGINAL;
unchanged members keep their original compressed bytes. Refuses a UTF-8 BOM,
CRLF-to-LF conversion, and edits that add structural errors.")]
    Pack {
        directory: PathBuf,
        /// The .dsd the directory was extracted from.
        original: PathBuf,
        #[command(flatten)]
        write: WriteArgs,
    },
    /// Report the version and release target; --check compares with the latest release.
    #[command(after_help = crate::help::VERSION_AFTER)]
    Version {
        /// Compare with the latest stable GitHub release, even when no update exists.
        #[arg(long)]
        check: bool,
    },
    /// Replace this receipt-managed installation with the latest stable release.
    #[command(after_help = crate::help::UPGRADE_AFTER)]
    Upgrade {
        /// Report whether a newer release is available without installing it.
        #[arg(long)]
        check: bool,
    },
}

impl Command {
    pub const fn name(&self) -> &'static str {
        match self {
            Self::Inspect { .. } => "inspect",
            Self::Outline { .. } => "outline",
            Self::Read { .. } => "read",
            Self::Table { .. } => "table",
            Self::Find { .. } => "find",
            Self::Check { .. } => "check",
            Self::Compare { .. } => "compare",
            Self::Diff { .. } => "diff",
            Self::SetCell { .. } => "set-cell",
            Self::Replace { .. } => "replace",
            Self::CopyRow { .. } => "copy-row",
            Self::DeleteRow { .. } => "delete-row",
            Self::CopyCol { .. } => "copy-col",
            Self::DeleteCol { .. } => "delete-col",
            Self::CopyTable { .. } => "copy-table",
            Self::DeleteTable { .. } => "delete-table",
            Self::InsertParagraph { .. } => "insert-paragraph",
            Self::DeleteParagraph { .. } => "delete-paragraph",
            Self::Apply { .. } => "apply",
            Self::Extract { .. } => "extract",
            Self::Pack { .. } => "pack",
            Self::Version { .. } => "version",
            Self::Upgrade { .. } => "upgrade",
        }
    }

    /// Whether a successful run may carry an incidental release advisory.
    /// `version` and `upgrade` report release state themselves.
    pub const fn checks_release(&self) -> bool {
        !matches!(self, Self::Version { .. } | Self::Upgrade { .. })
    }
}
