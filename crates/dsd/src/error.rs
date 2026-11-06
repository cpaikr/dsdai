use serde::Serialize;

use crate::check::Issue;

/// Stable failure categories. The CLI exposes them as `error.code`; the
/// message is for people and may change between versions.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum ErrorCode {
    /// An input path does not exist or is not a regular file.
    FileNotFound,
    /// The file is not a ZIP-based `.dsd`, or its container is unsafe or corrupt.
    InvalidArchive,
    /// `contents.xml` or `meta.xml` is not well-formed or lacks a required part.
    InvalidXml,
    /// A locator, option value, or edit field is out of range or malformed.
    InvalidArgument,
    /// The requested text does not occur where it was looked for.
    TextNotFound,
    /// A guarded edit found a different number of occurrences than expected.
    CountMismatch,
    /// The edit would depart from the form's rules (row locks, spans, units,
    /// mixed content) and was refused.
    EditRefused,
    /// The edit target depends on elements or a schema the tool has not verified.
    UnverifiedContent,
    /// The edit would add structural errors, so nothing was written.
    StructuralError,
    /// The output path exists, or is the input, and replacing it was not authorized.
    OutputExists,
    /// Reading or writing the filesystem failed.
    IoError,
    /// `check` found error-level issues.
    CheckFailed,
}

impl ErrorCode {
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::FileNotFound => "file_not_found",
            Self::InvalidArchive => "invalid_archive",
            Self::InvalidXml => "invalid_xml",
            Self::InvalidArgument => "invalid_argument",
            Self::TextNotFound => "text_not_found",
            Self::CountMismatch => "count_mismatch",
            Self::EditRefused => "edit_refused",
            Self::UnverifiedContent => "unverified_content",
            Self::StructuralError => "structural_error",
            Self::OutputExists => "output_exists",
            Self::IoError => "io_error",
            Self::CheckFailed => "check_failed",
        }
    }
}

/// A failure the caller can act on.
#[derive(Debug, Clone, thiserror::Error)]
#[error("{message}")]
pub struct Error {
    pub code: ErrorCode,
    pub message: String,
    /// What to do next, when there is a specific recovery.
    pub hint: Option<String>,
    /// Issues behind a `StructuralError`.
    pub issues: Vec<Issue>,
}

impl Error {
    pub fn new(code: ErrorCode, message: impl Into<String>) -> Self {
        Self { code, message: message.into(), hint: None, issues: Vec::new() }
    }

    #[must_use]
    pub fn with_hint(mut self, hint: impl Into<String>) -> Self {
        self.hint = Some(hint.into());
        self
    }

    #[must_use]
    pub fn with_issues(mut self, issues: Vec<Issue>) -> Self {
        self.issues = issues;
        self
    }

    /// Prefix the message with context such as the failing edit's position.
    #[must_use]
    pub fn context(mut self, prefix: &str) -> Self {
        self.message = format!("{prefix}: {}", self.message);
        self
    }

    pub fn io(path: &std::path::Path, err: &std::io::Error) -> Self {
        Self::new(ErrorCode::IoError, format!("{}: {err}", path.display()))
    }

    pub fn invalid(message: impl Into<String>) -> Self {
        Self::new(ErrorCode::InvalidArgument, message)
    }

    pub fn refused(message: impl Into<String>) -> Self {
        Self::new(ErrorCode::EditRefused, message)
    }
}

pub type Result<T, E = Error> = std::result::Result<T, E>;

/// The recovery route for edits the commands refuse.
pub const RAW_ROUTE: &str =
    "edit contents.xml as text instead (extract -> edit -> pack, then check --against and diff)";
