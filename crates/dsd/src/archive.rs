//! The ZIP container, read and written without normalization.
//!
//! DART Editor stores image member names as CP949 bytes without the ZIP UTF-8
//! flag, and it may mis-handle archives a generic tool has rewritten. This
//! module keeps every member's raw name bytes, flags, timestamps, attributes,
//! and extra fields, and copies unchanged members as their original
//! compressed bytes, so an unchanged archive is rewritten byte for byte.

use std::collections::{BTreeMap, HashSet};
use std::io::{Read, Write};
use std::path::{Path, PathBuf};

use crate::error::{Error, ErrorCode, Result};

pub const CONTENTS: &str = "contents.xml";
pub const META: &str = "meta.xml";

pub const MAX_ARCHIVE_BYTES: u64 = 200 * 1024 * 1024;
pub const MAX_MEMBER_BYTES: u64 = 200 * 1024 * 1024;
pub const MAX_MEMBERS: usize = 2000;
/// Bound on the declared decompressed size of all members together.
pub const MAX_TOTAL_BYTES: u64 = 512 * 1024 * 1024;

const FLAG_ENCRYPTED: u16 = 0x0001;
const FLAG_DATA_DESCRIPTOR: u16 = 0x0008;
const FLAG_UTF8: u16 = 0x0800;
const METHOD_STORED: u16 = 0;
const METHOD_DEFLATED: u16 = 8;
const ZIP64_EXTRA: u16 = 0x0001;

/// How a member name's raw bytes were decoded for display and matching.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum NameEncoding {
    Utf8,
    Ascii,
    Cp949,
    /// The bytes are neither ASCII nor CP949; shown through the ZIP default code page.
    Cp437,
}

impl NameEncoding {
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::Utf8 => "utf-8",
            Self::Ascii => "ascii",
            Self::Cp949 => "cp949",
            Self::Cp437 => "cp437",
        }
    }
}

/// One archive member with its stored layout and decompressed data.
#[derive(Debug, Clone)]
pub struct Member {
    pub raw_name: Vec<u8>,
    pub name: String,
    pub name_encoding: NameEncoding,
    pub version_made_by: u16,
    pub version_needed: u16,
    pub flags: u16,
    pub method: u16,
    pub dos_time: u16,
    pub dos_date: u16,
    pub crc: u32,
    pub size: u64,
    pub internal_attr: u16,
    pub external_attr: u32,
    pub central_extra: Vec<u8>,
    pub local_extra: Vec<u8>,
    pub comment: Vec<u8>,
    /// The compressed bytes exactly as stored.
    pub payload: Vec<u8>,
    pub data: Vec<u8>,
}

impl Member {
    pub const fn utf8_flag(&self) -> bool {
        self.flags & FLAG_UTF8 != 0
    }

    pub const fn method_name(&self) -> &'static str {
        match self.method {
            METHOD_STORED => "stored",
            METHOD_DEFLATED => "deflate",
            _ => "other",
        }
    }
}

#[derive(Debug, Clone)]
pub struct Archive {
    pub path: PathBuf,
    pub members: Vec<Member>,
    pub comment: Vec<u8>,
}

impl Archive {
    pub fn file_name(&self) -> String {
        self.path.file_name().map_or_else(|| self.path.display().to_string(), |n| n.to_string_lossy().into_owned())
    }

    pub fn member(&self, name: &str) -> Option<&Member> {
        self.members.iter().find(|m| m.name == name)
    }

    pub fn get(&self, name: &str) -> Result<&Member> {
        self.member(name).ok_or_else(|| {
            Error::new(ErrorCode::InvalidArchive, format!("{}: missing required member {name:?}", self.file_name()))
        })
    }

    pub fn contents(&self) -> Result<&[u8]> {
        Ok(&self.get(CONTENTS)?.data)
    }

    pub fn meta(&self) -> Result<&[u8]> {
        Ok(&self.get(META)?.data)
    }

    pub fn images(&self) -> impl Iterator<Item = &Member> {
        self.members.iter().filter(|m| m.name != CONTENTS && m.name != META)
    }

    /// A copy whose named members carry new data; layout is unchanged.
    #[must_use]
    pub fn with_replacements(&self, replacements: &BTreeMap<String, Vec<u8>>) -> Self {
        let mut probe = self.clone();
        for m in &mut probe.members {
            if let Some(data) = replacements.get(&m.name) {
                m.data.clone_from(data);
            }
        }
        probe
    }
}

fn u16_at(buf: &[u8], at: usize) -> u16 {
    u16::from_le_bytes([buf[at], buf[at + 1]])
}

fn u32_at(buf: &[u8], at: usize) -> u32 {
    u32::from_le_bytes([buf[at], buf[at + 1], buf[at + 2], buf[at + 3]])
}

fn u64_at(buf: &[u8], at: usize) -> u64 {
    let mut bytes = [0u8; 8];
    bytes.copy_from_slice(&buf[at..at + 8]);
    u64::from_le_bytes(bytes)
}

const CP437_HIGH: &str = "ÇüéâäàåçêëèïîìÄÅÉæÆôöòûùÿÖÜ¢£¥₧ƒáíóúñÑªº¿⌐¬½¼¡«»░▒▓│┤╡╢╖╕╣║╗╝╜╛┐└┴┬├─┼╞╟╚╔╩╦╠═╬╧╨╤╥╙╘╒╓╫╪┘┌█▄▌▐▀αßΓπΣσµτΦΘΩδ∞φε∩≡±≥≤⌠⌡÷≈°∙·√ⁿ²■\u{a0}";

fn decode_cp437(raw: &[u8]) -> String {
    let high: Vec<char> = CP437_HIGH.chars().collect();
    raw.iter().map(|&b| if b < 0x80 { char::from(b) } else { high[usize::from(b - 0x80)] }).collect()
}

fn decode_name(raw: &[u8], utf8_flag: bool) -> Option<(String, NameEncoding)> {
    if utf8_flag {
        return String::from_utf8(raw.to_vec()).ok().map(|s| (s, NameEncoding::Utf8));
    }
    if raw.is_ascii() {
        return Some((String::from_utf8_lossy(raw).into_owned(), NameEncoding::Ascii));
    }
    if let Some(text) = encoding_rs::EUC_KR.decode_without_bom_handling_and_without_replacement(raw) {
        return Some((text.into_owned(), NameEncoding::Cp949));
    }
    Some((decode_cp437(raw), NameEncoding::Cp437))
}

fn unsafe_name(name: &str) -> bool {
    let bytes = name.as_bytes();
    name.starts_with(['/', '\\'])
        || name.split(['/', '\\']).any(|part| part == "..")
        || (bytes.len() >= 2 && bytes[0].is_ascii_alphabetic() && bytes[1] == b':')
        || name.contains('\0')
}

/// Locate the end-of-central-directory record: (offset, record bytes).
fn find_eocd(buf: &[u8]) -> Option<usize> {
    if buf.len() < 22 {
        return None;
    }
    let floor = buf.len().saturating_sub(22 + 0xFFFF);
    (floor..=buf.len() - 22)
        .rev()
        .find(|&at| buf[at..at + 4] == *b"PK\x05\x06" && at + 22 + usize::from(u16_at(buf, at + 20)) <= buf.len())
}

/// Read and validate a `.dsd` archive.
pub fn read_archive(path: &Path) -> Result<Archive> {
    if !path.is_file() {
        return Err(Error::new(ErrorCode::FileNotFound, format!("{}: file not found", path.display())));
    }
    let shown = path.file_name().map_or_else(|| path.display().to_string(), |n| n.to_string_lossy().into_owned());
    let size = std::fs::metadata(path).map_err(|e| Error::io(path, &e))?.len();
    if size > MAX_ARCHIVE_BYTES {
        return Err(Error::new(
            ErrorCode::InvalidArchive,
            format!("{shown}: archive exceeds {MAX_ARCHIVE_BYTES} bytes"),
        ));
    }
    let buf = std::fs::read(path).map_err(|e| Error::io(path, &e))?;
    let bad = |why: String| Error::new(ErrorCode::InvalidArchive, format!("{shown}: {why}"));
    let not_zip = |why: &str| bad(format!("not a ZIP-based .dsd ({why})"));

    let eocd = find_eocd(&buf).ok_or_else(|| not_zip("no end-of-central-directory record"))?;
    if eocd >= 20 && buf[eocd - 20..eocd - 16] == *b"PK\x06\x07" {
        return Err(bad("ZIP64 archives are not supported".into()));
    }
    let disk_entries = usize::from(u16_at(&buf, eocd + 8));
    let total_entries = usize::from(u16_at(&buf, eocd + 10));
    let cd_size = u32_at(&buf, eocd + 12) as usize;
    let cd_offset = u32_at(&buf, eocd + 16) as usize;
    let comment_len = usize::from(u16_at(&buf, eocd + 20));
    if disk_entries != total_entries || u16_at(&buf, eocd + 4) != 0 || u16_at(&buf, eocd + 6) != 0 {
        return Err(bad("multi-disk archives are not supported".into()));
    }
    // Bytes prepended to the archive shift every offset by the same amount.
    let concat = eocd
        .checked_sub(cd_size)
        .and_then(|v| v.checked_sub(cd_offset))
        .ok_or_else(|| not_zip("central directory out of range"))?;
    let comment = buf[eocd + 22..eocd + 22 + comment_len].to_vec();
    if total_entries > MAX_MEMBERS {
        return Err(bad(format!("too many members ({total_entries})")));
    }

    let mut members = Vec::with_capacity(total_entries);
    let mut total_size: u64 = 0;
    let mut seen = HashSet::new();
    let mut at = cd_offset + concat;
    for _ in 0..total_entries {
        if at + 46 > eocd || buf[at..at + 4] != *b"PK\x01\x02" {
            return Err(not_zip("bad central directory entry"));
        }
        let version_made_by = u16_at(&buf, at + 4);
        let version_needed = u16_at(&buf, at + 6);
        let flags = u16_at(&buf, at + 8);
        let method = u16_at(&buf, at + 10);
        let dos_time = u16_at(&buf, at + 12);
        let dos_date = u16_at(&buf, at + 14);
        let crc = u32_at(&buf, at + 16);
        let mut compressed_size = u64::from(u32_at(&buf, at + 20));
        let mut size = u64::from(u32_at(&buf, at + 24));
        let name_len = usize::from(u16_at(&buf, at + 28));
        let extra_len = usize::from(u16_at(&buf, at + 30));
        let comment_len = usize::from(u16_at(&buf, at + 32));
        let internal_attr = u16_at(&buf, at + 36);
        let external_attr = u32_at(&buf, at + 38);
        let mut header_offset = u64::from(u32_at(&buf, at + 42));
        let fields = at + 46;
        let next = fields + name_len + extra_len + comment_len;
        if next > eocd {
            return Err(not_zip("central directory entry out of range"));
        }
        let raw_name = buf[fields..fields + name_len].to_vec();
        let central_extra = buf[fields + name_len..fields + name_len + extra_len].to_vec();
        let member_comment = buf[fields + name_len + extra_len..next].to_vec();
        at = next;

        apply_zip64_extra(&central_extra, &mut size, &mut compressed_size, &mut header_offset);
        let (name, name_encoding) = decode_name(&raw_name, flags & FLAG_UTF8 != 0)
            .ok_or_else(|| bad("member name flagged UTF-8 is not valid UTF-8".into()))?;
        if flags & FLAG_ENCRYPTED != 0 {
            return Err(bad(format!("encrypted member {name:?} is not supported")));
        }
        if unsafe_name(&name) {
            return Err(bad(format!("unsafe member path {name:?}")));
        }
        if !seen.insert(name.to_lowercase()) {
            return Err(bad(format!("duplicate member {name:?}")));
        }
        if size > MAX_MEMBER_BYTES {
            return Err(bad(format!("member {name:?} is too large")));
        }
        total_size = total_size.saturating_add(size);
        if total_size > MAX_TOTAL_BYTES {
            return Err(bad(format!("members decompress to more than {MAX_TOTAL_BYTES} bytes")));
        }
        if method != METHOD_STORED && method != METHOD_DEFLATED {
            return Err(bad(format!("member {name:?} uses unsupported compression {method}")));
        }

        let local = usize::try_from(header_offset).unwrap_or(usize::MAX).saturating_add(concat);
        if local.saturating_add(30) > buf.len() || buf[local..local + 4] != *b"PK\x03\x04" {
            return Err(bad(format!("bad local header for {name:?}")));
        }
        let local_name_len = usize::from(u16_at(&buf, local + 26));
        let local_extra_len = usize::from(u16_at(&buf, local + 28));
        let data_at = local + 30 + local_name_len + local_extra_len;
        let compressed = usize::try_from(compressed_size).unwrap_or(usize::MAX);
        if data_at.saturating_add(compressed) > buf.len() {
            return Err(bad(format!("member {name:?} is truncated")));
        }
        if buf[local + 30..local + 30 + local_name_len] != raw_name[..] {
            return Err(bad(format!("member {name:?}: file name in directory and local header differ")));
        }
        let local_extra = buf[local + 30 + local_name_len..data_at].to_vec();
        let payload = buf[data_at..data_at + compressed].to_vec();
        let data =
            decompress(method, &payload, size).map_err(|why| bad(format!("member {name:?} is corrupt ({why})")))?;
        if crc32fast::hash(&data) != crc {
            return Err(bad(format!("member {name:?} is corrupt (bad CRC-32)")));
        }
        members.push(Member {
            raw_name,
            name,
            name_encoding,
            version_made_by,
            version_needed,
            flags,
            method,
            dos_time,
            dos_date,
            crc,
            size,
            internal_attr,
            external_attr,
            central_extra,
            local_extra,
            comment: member_comment,
            payload,
            data,
        });
    }
    Ok(Archive { path: path.to_path_buf(), members, comment })
}

fn apply_zip64_extra(extra: &[u8], size: &mut u64, compressed: &mut u64, offset: &mut u64) {
    let mut pos = 0;
    while pos + 4 <= extra.len() {
        let tag = u16_at(extra, pos);
        let len = usize::from(u16_at(extra, pos + 2));
        let body = &extra[pos + 4..(pos + 4 + len).min(extra.len())];
        if tag == ZIP64_EXTRA {
            let mut at = 0;
            for field in [&mut *size, &mut *compressed, &mut *offset] {
                if *field == u64::from(u32::MAX) && at + 8 <= body.len() {
                    *field = u64_at(body, at);
                    at += 8;
                }
            }
        }
        pos += 4 + len;
    }
}

fn decompress(method: u16, payload: &[u8], size: u64) -> std::result::Result<Vec<u8>, String> {
    let data = if method == METHOD_STORED {
        payload.to_vec()
    } else {
        let mut out = Vec::with_capacity(usize::try_from(size).unwrap_or(0).min(64 << 20));
        flate2::read::DeflateDecoder::new(payload)
            .take(MAX_MEMBER_BYTES + 1)
            .read_to_end(&mut out)
            .map_err(|e| e.to_string())?;
        out
    };
    if data.len() as u64 != size {
        return Err(format!("size {} does not match the declared {size}", data.len()));
    }
    Ok(data)
}

/// Drop ZIP64 (0x0001) records; the writer emits classic headers only.
fn without_zip64(extra: &[u8]) -> Vec<u8> {
    let mut out = Vec::with_capacity(extra.len());
    let mut pos = 0;
    while pos + 4 <= extra.len() {
        let tag = u16_at(extra, pos);
        let end = (pos + 4 + usize::from(u16_at(extra, pos + 2))).min(extra.len());
        if tag != ZIP64_EXTRA {
            out.extend_from_slice(&extra[pos..end]);
        }
        pos = end;
    }
    out
}

fn deflate(data: &[u8]) -> Vec<u8> {
    let mut enc = flate2::write::DeflateEncoder::new(Vec::new(), flate2::Compression::new(6));
    enc.write_all(data).expect("writing to memory cannot fail");
    enc.finish().expect("writing to memory cannot fail")
}

fn len_u16(len: usize, what: &str) -> Result<u16> {
    u16::try_from(len)
        .map_err(|_| Error::new(ErrorCode::InvalidArchive, format!("{what} is too long for a ZIP header")))
}

fn len_u32(len: u64) -> Result<u32> {
    u32::try_from(len).map_err(|_| Error::new(ErrorCode::InvalidArchive, "member too large for a non-ZIP64 archive"))
}

/// Serialize `source` with the named members' data replaced.
///
/// Member order, raw name bytes, general-purpose flags (minus data
/// descriptors), timestamps, attributes, extra fields (minus ZIP64), and
/// comments are preserved. Unchanged members keep their original compressed
/// bytes.
pub fn archive_bytes(source: &Archive, replacements: &BTreeMap<String, Vec<u8>>) -> Result<Vec<u8>> {
    let unknown: Vec<&String> = replacements.keys().filter(|k| !source.members.iter().any(|m| &m.name == *k)).collect();
    if !unknown.is_empty() {
        return Err(Error::invalid(format!("cannot replace missing members: {unknown:?}")));
    }
    let mut out = Vec::new();
    let mut central = Vec::new();
    for m in &source.members {
        let local_extra = without_zip64(&m.local_extra);
        let central_extra = without_zip64(&m.central_extra);
        let (payload, crc, size) = match replacements.get(&m.name) {
            Some(data) if *data != m.data => {
                let payload = if m.method == METHOD_DEFLATED { deflate(data) } else { data.clone() };
                (payload, crc32fast::hash(data), data.len() as u64)
            }
            _ => (m.payload.clone(), m.crc, m.size),
        };
        let size = len_u32(size)?;
        let compressed = len_u32(payload.len() as u64)?;
        let offset = len_u32(out.len() as u64)?;
        let flags = m.flags & !FLAG_DATA_DESCRIPTOR;
        let name_len = len_u16(m.raw_name.len(), "member name")?;

        out.extend_from_slice(b"PK\x03\x04");
        for v in [m.version_needed, flags, m.method, m.dos_time, m.dos_date] {
            out.extend_from_slice(&v.to_le_bytes());
        }
        for v in [crc, compressed, size] {
            out.extend_from_slice(&v.to_le_bytes());
        }
        out.extend_from_slice(&name_len.to_le_bytes());
        out.extend_from_slice(&len_u16(local_extra.len(), "extra field")?.to_le_bytes());
        out.extend_from_slice(&m.raw_name);
        out.extend_from_slice(&local_extra);
        out.extend_from_slice(&payload);

        central.extend_from_slice(b"PK\x01\x02");
        for v in [m.version_made_by, m.version_needed, flags, m.method, m.dos_time, m.dos_date] {
            central.extend_from_slice(&v.to_le_bytes());
        }
        for v in [crc, compressed, size] {
            central.extend_from_slice(&v.to_le_bytes());
        }
        for v in [
            name_len,
            len_u16(central_extra.len(), "extra field")?,
            len_u16(m.comment.len(), "member comment")?,
            0,
            m.internal_attr,
        ] {
            central.extend_from_slice(&v.to_le_bytes());
        }
        central.extend_from_slice(&m.external_attr.to_le_bytes());
        central.extend_from_slice(&offset.to_le_bytes());
        central.extend_from_slice(&m.raw_name);
        central.extend_from_slice(&central_extra);
        central.extend_from_slice(&m.comment);
    }
    let count = len_u16(source.members.len(), "member count")?;
    let cd_offset = len_u32(out.len() as u64)?;
    let cd_size = len_u32(central.len() as u64)?;
    out.extend_from_slice(&central);
    out.extend_from_slice(b"PK\x05\x06");
    for v in [0u16, 0, count, count] {
        out.extend_from_slice(&v.to_le_bytes());
    }
    out.extend_from_slice(&cd_size.to_le_bytes());
    out.extend_from_slice(&cd_offset.to_le_bytes());
    out.extend_from_slice(&len_u16(source.comment.len(), "archive comment")?.to_le_bytes());
    out.extend_from_slice(&source.comment);
    Ok(out)
}

pub fn same_file(a: &Path, b: &Path) -> bool {
    match (a.canonicalize(), b.canonicalize()) {
        (Ok(x), Ok(y)) => x == y,
        _ => false,
    }
}

/// Refuse to replace an existing file, or an input, unless forced.
pub fn guard_output(out: &Path, force: bool, inputs: &[&Path]) -> Result<()> {
    if out.exists() && !force {
        return Err(Error::new(ErrorCode::OutputExists, format!("{}: already exists", out.display()))
            .with_hint("pass --force only if replacing it is authorized"));
    }
    if !force && inputs.iter().any(|src| same_file(out, src)) {
        return Err(Error::new(
            ErrorCode::OutputExists,
            format!("{}: refusing to overwrite the input in place without --force", out.display()),
        ));
    }
    Ok(())
}

/// Write through a synced temporary file in the destination directory, then rename.
pub fn atomic_write(out: &Path, data: &[u8]) -> Result<()> {
    let parent = match out.parent() {
        Some(p) if !p.as_os_str().is_empty() => p.to_path_buf(),
        _ => PathBuf::from("."),
    };
    std::fs::create_dir_all(&parent).map_err(|e| Error::io(&parent, &e))?;
    let stamp = std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH).map_or(0, |d| d.as_nanos());
    let mut attempt = 0u32;
    let (tmp, mut file) = loop {
        let candidate = parent.join(format!(".dsd-{}-{stamp}-{attempt}.tmp", std::process::id()));
        match std::fs::OpenOptions::new().write(true).create_new(true).open(&candidate) {
            Ok(file) => break (candidate, file),
            Err(e) if e.kind() == std::io::ErrorKind::AlreadyExists && attempt < 100 => attempt += 1,
            Err(e) => return Err(Error::io(&candidate, &e)),
        }
    };
    let result = file.write_all(data).and_then(|()| file.sync_all());
    drop(file);
    if let Err(e) = result.and_then(|()| std::fs::rename(&tmp, out)) {
        let _ = std::fs::remove_file(&tmp);
        return Err(Error::io(out, &e));
    }
    Ok(())
}

/// Write `source` to `out` with member replacements, guarding the destination.
pub fn write_archive(
    source: &Archive,
    replacements: &BTreeMap<String, Vec<u8>>,
    out: &Path,
    force: bool,
) -> Result<()> {
    guard_output(out, force, &[&source.path])?;
    let bytes = archive_bytes(source, replacements)?;
    atomic_write(out, &bytes)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn cp437_table_covers_the_high_half() {
        assert_eq!(CP437_HIGH.chars().count(), 128);
        assert_eq!(decode_cp437(&[0x80, 0xff, b'a']), "Ç\u{a0}a");
    }

    #[test]
    fn names_decode_in_preference_order() {
        assert_eq!(decode_name(b"a.jpg", false).unwrap().1, NameEncoding::Ascii);
        let (name, enc) = decode_name(&[0xb0, 0xa1, b'.', b'j'], false).unwrap();
        assert_eq!((name.as_str(), enc), ("가.j", NameEncoding::Cp949));
        assert_eq!(decode_name("가".as_bytes(), true).unwrap().1, NameEncoding::Utf8);
        assert!(decode_name(&[0xff], true).is_none());
    }

    #[test]
    fn unsafe_names_are_detected() {
        for name in ["/a", "\\a", "a/../b", "C:x", "a\0b"] {
            assert!(unsafe_name(name), "{name}");
        }
        assert!(!unsafe_name("images/a..b.jpg"));
    }
}
