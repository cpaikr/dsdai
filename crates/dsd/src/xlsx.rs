//! Serialize a [`Workbook`] model to xlsx bytes with `rust_xlsxwriter`.
//!
//! Everything a cell holds or where it sits is decided in [`crate::sheets`];
//! this module only maps roles to styles. No file I/O, and no clock: the
//! document creation time is pinned so the same model yields the same bytes.

use rust_xlsxwriter::{
    Color, DocProperties, ExcelDateTime, Format, FormatAlign, FormatBorder, Url, Workbook as Book, Worksheet, XlsxError,
};

use crate::doc::Doc;
use crate::error::{Error, Result};
use crate::sheets::{Cell, Role, Sheet, Value, Workbook, workbook};

const HEADER_FILL: u32 = 0x00D9_D9D9;
const MARKER_GREY: u32 = 0x0080_8080;

/// The workbook model and its xlsx bytes.
pub fn export(doc: &Doc) -> Result<(Workbook, Vec<u8>)> {
    let model = workbook(doc)?;
    let bytes = write(&model)?;
    Ok((model, bytes))
}

/// xlsx bytes for a model.
pub fn write(model: &Workbook) -> Result<Vec<u8>> {
    write_book(model).map_err(|e| Error::invalid(format!("the workbook could not be written: {e}")))
}

/// A sheet name quoted for formulas and internal links.
pub fn quoted(name: &str) -> String {
    format!("'{}'", name.replace('\'', "''"))
}

fn write_book(model: &Workbook) -> std::result::Result<Vec<u8>, XlsxError> {
    let mut book = Book::new();
    // A fixed creation time (and with it the modified time) keeps output reproducible.
    let created = ExcelDateTime::from_ymd(2000, 1, 1)?;
    book.set_properties(&DocProperties::new().set_creation_datetime(&created));
    for sheet in &model.sheets {
        let ws = book.add_worksheet();
        write_sheet(ws, sheet, model)?;
    }
    for p in &model.placements {
        let sheet = &model.sheets[p.sheet].name;
        book.define_name(p.name(), &format!("={}!{}", quoted(sheet), p.absolute_range()))?;
    }
    book.save_to_buffer()
}

fn write_sheet(ws: &mut Worksheet, sheet: &Sheet, model: &Workbook) -> std::result::Result<(), XlsxError> {
    ws.set_name(&sheet.name)?;
    if sheet.frozen_rows > 0 {
        ws.set_freeze_panes(sheet.frozen_rows, 0)?;
    }
    for (&col, &px) in &sheet.widths {
        ws.set_column_width_pixels(col, u32::from(px))?;
    }
    // Merges first: each writes its range's style; the value then lands on its first cell.
    for m in &sheet.merges {
        let origin = sheet.cells.iter().find(|c| (c.row, c.col) == (m.first_row, m.first_col));
        let format = origin.map_or_else(Format::new, |c| format_of(c, None));
        ws.merge_range(m.first_row, m.first_col, m.last_row, m.last_col, "", &format)?;
    }
    for cell in &sheet.cells {
        match &cell.value {
            None => {
                ws.write_blank(cell.row, cell.col, &format_of(cell, None))?;
            }
            Some(Value::Text(text)) => {
                ws.write_string_with_format(cell.row, cell.col, text, &format_of(cell, None))?;
            }
            Some(Value::Number { value, format }) => {
                ws.write_number_with_format(cell.row, cell.col, *value, &format_of(cell, Some(format)))?;
            }
            Some(Value::Link { sheet: target, text }) => {
                let url = Url::new(format!("internal:{}!A1", quoted(&model.sheets[*target].name))).set_text(text);
                ws.write_url(cell.row, cell.col, url)?;
            }
        }
    }
    Ok(())
}

fn format_of(cell: &Cell, num_format: Option<&str>) -> Format {
    let mut f = Format::new();
    match cell.role {
        Role::Plain => {}
        Role::Title | Role::Bold => f = f.set_bold(),
        Role::Header => {
            f = f
                .set_bold()
                .set_align(FormatAlign::Center)
                .set_align(FormatAlign::VerticalCenter)
                .set_background_color(Color::RGB(HEADER_FILL));
        }
        Role::Marker => f = f.set_font_color(Color::RGB(MARKER_GREY)).set_font_size(8),
    }
    if cell.border {
        f = f.set_border(FormatBorder::Thin);
    }
    if cell.wrap {
        f = f.set_text_wrap();
    }
    if let Some(n) = num_format {
        f = f.set_num_format(n);
    }
    f
}

#[cfg(test)]
mod tests {
    use super::*;

    fn doc(body: &str) -> Doc {
        let xml = format!(
            "<?xml version=\"1.0\" encoding=\"utf-8\"?>\n<DOCUMENT xmlns:xsi=\"http://www.w3.org/2001/XMLSchema-instance\" xsi:noNamespaceSchemaLocation=\"dart4.xsd\"><BODY>{body}</BODY></DOCUMENT>"
        );
        Doc::parse(xml.into_bytes()).unwrap()
    }

    const BODY: &str = "<SECTION-1><TITLE>시험 '구역'</TITLE><P>글</P><TABLE BORDER=\"1\"><COLGROUP><COL WIDTH=\"80\"/><COL WIDTH=\"80\"/></COLGROUP><TBODY><TR><TH COLSPAN=\"2\">머리</TH></TR><TR><TD>가</TD><TD>(1,234)</TD></TR></TBODY></TABLE></SECTION-1>";

    #[test]
    fn same_model_same_bytes() {
        let (_, a) = export(&doc(BODY)).unwrap();
        let (_, b) = export(&doc(BODY)).unwrap();
        assert_eq!(a, b);
        assert!(a.starts_with(b"PK\x03\x04"));
    }

    #[test]
    fn names_with_apostrophes_are_quoted() {
        assert_eq!(quoted("시험 '구역'"), "'시험 ''구역'''");
        let (model, bytes) = export(&doc(BODY)).unwrap();
        assert_eq!(model.sheets[1].name, "시험 '구역", "a trailing apostrophe is not allowed");
        assert!(!bytes.is_empty());
    }
}
