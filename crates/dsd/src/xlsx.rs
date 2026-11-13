//! Serialize a [`Workbook`] model to xlsx bytes with `rust_xlsxwriter`.
//!
//! Everything a cell holds or where it sits is decided in [`crate::sheets`];
//! this module only maps roles to styles. No file I/O, and no clock: the
//! document creation time is pinned so the same model yields the same bytes.

use std::collections::{BTreeMap, HashMap};

use rust_xlsxwriter::{
    Color, DocProperties, ExcelDateTime, Format, FormatAlign, FormatBorder, Url, Workbook as Book, Worksheet, XlsxError,
};

use crate::doc::Doc;
use crate::error::{Error, Result};
use crate::sheets::{Cell, Role, Sheet, Value, Workbook, workbook};

const HEADER_FILL: u32 = 0x00D9_D9D9;
/// Excel's default row height for the default 11 pt font.
const LINE_HEIGHT_PT: f64 = 15.0;
/// Excel's default column width, and glyph width estimates for the default
/// 11 pt font: Hangul and other wide characters, and everything else.
const DEFAULT_COL_PX: u16 = 64;
const WIDE_GLYPH_PX: u32 = 15;
const NARROW_GLYPH_PX: u32 = 8;
const CELL_PADDING_PX: u32 = 8;
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
    let at: HashMap<(u32, u16), &Cell> = sheet.cells.iter().map(|c| ((c.row, c.col), c)).collect();
    // Merges first: each writes its range's style; the value then lands on its first cell.
    for m in &sheet.merges {
        let origin = at.get(&(m.first_row, m.first_col));
        let format = origin.map_or_else(Format::new, |c| format_of(c, None));
        ws.merge_range(m.first_row, m.first_col, m.last_row, m.last_col, "", &format)?;
    }
    // Excel fits row heights to wrapped text, but not inside a merged range: give
    // a row that holds a multi-line merge the height of its tallest wrapped cell,
    // estimating how its lines also wrap at the merged width.
    let width = |first: u16, last: u16| {
        (first..=last).map(|c| u32::from(sheet.widths.get(&c).copied().unwrap_or(DEFAULT_COL_PX))).sum::<u32>()
    };
    let mut heights: BTreeMap<u32, usize> = BTreeMap::new();
    for m in sheet.merges.iter().filter(|m| m.first_row == m.last_row) {
        if let Some(c) = at.get(&(m.first_row, m.first_col)).filter(|c| c.wrap) {
            heights.insert(m.first_row, line_count(c, width(m.first_col, m.last_col)));
        }
    }
    let spans: HashMap<(u32, u16), u16> = sheet
        .merges
        .iter()
        .filter(|m| m.first_row == m.last_row)
        .map(|m| ((m.first_row, m.first_col), m.last_col))
        .collect();
    for c in sheet.cells.iter().filter(|c| c.wrap) {
        if let Some(h) = heights.get_mut(&c.row) {
            let last = spans.get(&(c.row, c.col)).copied().unwrap_or(c.col);
            *h = (*h).max(line_count(c, width(c.col, last)));
        }
    }
    for (&row, &lines) in &heights {
        ws.set_row_height(row, LINE_HEIGHT_PT * lines as f64)?;
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

/// Displayed lines of a wrapped cell `width_px` wide: its own lines, each
/// wrapping by an estimate of the default font's glyph widths (on the wide
/// side, so text is not clipped).
fn line_count(cell: &Cell, width_px: u32) -> usize {
    let Some(Value::Text(text)) = &cell.value else { return 1 };
    let usable = width_px.saturating_sub(CELL_PADDING_PX).max(NARROW_GLYPH_PX);
    text.split('\n')
        .map(|line| {
            let px: u32 =
                line.chars().map(|c| if u32::from(c) >= 0x1100 { WIDE_GLYPH_PX } else { NARROW_GLYPH_PX }).sum();
            px.div_ceil(usable).max(1) as usize
        })
        .sum()
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

    /// One member of a zip, located through its central directory.
    fn member(zip: &[u8], name: &str) -> String {
        let u16_at = |i: usize| usize::from(u16::from_le_bytes([zip[i], zip[i + 1]]));
        let u32_at = |i: usize| u32::from_le_bytes(zip[i..i + 4].try_into().unwrap()) as usize;
        let eocd = zip.windows(4).rposition(|w| w == b"PK\x05\x06").unwrap();
        let mut at = u32_at(eocd + 16);
        for _ in 0..u16_at(eocd + 10) {
            let (method, csize) = (u16_at(at + 10), u32_at(at + 20));
            let (nlen, xlen, clen, offset) = (u16_at(at + 28), u16_at(at + 30), u16_at(at + 32), u32_at(at + 42));
            if &zip[at + 46..at + 46 + nlen] == name.as_bytes() {
                let start = offset + 30 + u16_at(offset + 26) + u16_at(offset + 28);
                let data = &zip[start..start + csize];
                let mut out = String::new();
                if method == 8 {
                    std::io::Read::read_to_string(&mut flate2::read::DeflateDecoder::new(data), &mut out).unwrap();
                } else {
                    out = String::from_utf8(data.to_vec()).unwrap();
                }
                return out;
            }
            at += 46 + nlen + xlen + clen;
        }
        panic!("no member {name}");
    }

    #[test]
    fn rows_with_a_multi_line_merge_get_a_height() {
        let body = "<SECTION-1><TITLE>높이</TITLE><TABLE><COLGROUP><COL/><COL/></COLGROUP><TBODY><TR><TD COLSPAN=\"2\">가&amp;cr;나&amp;cr;다</TD></TR><TR><TD>a&amp;cr;b</TD><TD>c</TD></TR></TBODY></TABLE></SECTION-1>";
        let (model, bytes) = export(&doc(body)).unwrap();
        let row = model.placements[0].row + 1;
        let sheet = member(&bytes, "xl/worksheets/sheet2.xml");
        assert!(sheet.contains(&format!("<row r=\"{row}\" spans=\"1:3\" ht=\"45\" customHeight=\"1\">")), "{sheet}");
        assert!(!sheet.contains(&format!("<row r=\"{}\" spans=\"1:2\" ht=", row + 1)), "unmerged rows autofit");
    }

    #[test]
    fn a_long_line_in_a_narrow_merge_counts_its_wrapped_lines() {
        // Two 50 px columns leave 92 px; 20 Hangul syllables need about 300 px (4 lines), plus one more line.
        let body = format!(
            "<SECTION-1><TITLE>폭</TITLE><TABLE><COLGROUP><COL WIDTH=\"50\"/><COL WIDTH=\"50\"/></COLGROUP><TBODY><TR><TD COLSPAN=\"2\">{}&amp;cr;끝</TD></TR></TBODY></TABLE></SECTION-1>",
            "가".repeat(20)
        );
        let (model, bytes) = export(&doc(&body)).unwrap();
        let row = model.placements[0].row + 1;
        let sheet = member(&bytes, "xl/worksheets/sheet2.xml");
        assert!(sheet.contains(&format!("<row r=\"{row}\" spans=\"1:3\" ht=\"75\" customHeight=\"1\">")), "{sheet}");
    }
}
