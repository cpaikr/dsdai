//! Read, check, and surgically edit DART Editor `.dsd` documents.
//!
//! A `.dsd` is a ZIP archive holding `contents.xml` (the document),
//! `meta.xml` (editor metadata and image inventory), and embedded images.
//! Nothing here re-serializes XML or re-zips with a generic writer: edits
//! replace exact byte ranges located by a positioned parser, and archives are
//! rewritten with their original member layout.

pub mod archive;
pub mod check;
pub mod diff;
pub mod difflib;
pub mod doc;
pub mod edit;
mod error;
pub mod schema;
pub mod text;
pub mod views;
pub mod xml;

pub use archive::{Archive, read_archive};
pub use check::{Issue, Level};
pub use doc::Doc;
pub use error::{Error, ErrorCode, RAW_ROUTE, Result};

/// Parse an archive's `contents.xml`.
pub fn document(arc: &Archive) -> Result<Doc> {
    Doc::parse(arc.contents()?.to_vec())
}
