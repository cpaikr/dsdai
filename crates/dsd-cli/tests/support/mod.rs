//! Shared helpers: the synthetic fixture, ZIP building, and process runs.
#![allow(dead_code, clippy::unused_self)]

use std::io::Write as _;
use std::path::{Path, PathBuf};
use std::process::Command;

use serde_json::Value;

/// Independently authored placeholder image; its member name is stored as
/// CP949 bytes without the ZIP UTF-8 flag, as DART Editor writes it.
pub const IMAGE_NAME: &str = "대표이사 확인서.jpg";
pub const IMAGE_BYTES: &[u8] = b"\xff\xd8\xff\xe0synthetic-jpeg-placeholder\xff\xd9";

pub fn repo() -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR")).join("../..").canonicalize().unwrap()
}

/// A fixture source file (stored with LF; documents use CRLF).
pub fn source(name: &str) -> String {
    std::fs::read_to_string(repo().join("fixtures/public/sample").join(name)).unwrap()
}

pub fn crlf(text: &str) -> Vec<u8> {
    text.replace("\r\n", "\n").replace('\n', "\r\n").into_bytes()
}

pub fn cp949(name: &str) -> Vec<u8> {
    encoding_rs::EUC_KR.encode(name).0.into_owned()
}

/// Deflate ZIP with raw name bytes, no UTF-8 flag, and a fixed DOS timestamp.
pub fn build_zip(members: &[(Vec<u8>, Vec<u8>)]) -> Vec<u8> {
    let (time, date): (u16, u16) = ((10 << 11) | (30 << 5), ((2026 - 1980) << 9) | (1 << 5) | 15);
    let mut out = Vec::new();
    let mut central = Vec::new();
    for (name, data) in members {
        let mut enc = flate2::write::DeflateEncoder::new(Vec::new(), flate2::Compression::new(6));
        enc.write_all(data).unwrap();
        let payload = enc.finish().unwrap();
        let crc = crc32fast::hash(data);
        let offset = u32::try_from(out.len()).unwrap();
        let (csize, size, nlen) = (
            u32::try_from(payload.len()).unwrap(),
            u32::try_from(data.len()).unwrap(),
            u16::try_from(name.len()).unwrap(),
        );
        out.extend_from_slice(b"PK\x03\x04");
        for v in [20u16, 0, 8, time, date] {
            out.extend_from_slice(&v.to_le_bytes());
        }
        for v in [crc, csize, size] {
            out.extend_from_slice(&v.to_le_bytes());
        }
        out.extend_from_slice(&nlen.to_le_bytes());
        out.extend_from_slice(&0u16.to_le_bytes());
        out.extend_from_slice(name);
        out.extend_from_slice(&payload);
        central.extend_from_slice(b"PK\x01\x02");
        for v in [20u16, 20, 0, 8, time, date] {
            central.extend_from_slice(&v.to_le_bytes());
        }
        for v in [crc, csize, size] {
            central.extend_from_slice(&v.to_le_bytes());
        }
        for v in [nlen, 0, 0, 0, 0] {
            central.extend_from_slice(&v.to_le_bytes());
        }
        central.extend_from_slice(&0x20u32.to_le_bytes());
        central.extend_from_slice(&offset.to_le_bytes());
        central.extend_from_slice(name);
    }
    let (count, cd_size, cd_offset) = (
        u16::try_from(members.len()).unwrap(),
        u32::try_from(central.len()).unwrap(),
        u32::try_from(out.len()).unwrap(),
    );
    out.extend_from_slice(&central);
    out.extend_from_slice(b"PK\x05\x06");
    for v in [0u16, 0, count, count] {
        out.extend_from_slice(&v.to_le_bytes());
    }
    out.extend_from_slice(&cd_size.to_le_bytes());
    out.extend_from_slice(&cd_offset.to_le_bytes());
    out.extend_from_slice(&0u16.to_le_bytes());
    out
}

pub fn members_with(contents: &[u8]) -> Vec<(Vec<u8>, Vec<u8>)> {
    vec![
        (b"contents.xml".to_vec(), contents.to_vec()),
        (b"meta.xml".to_vec(), crlf(&source("meta.xml"))),
        (cp949(IMAGE_NAME), IMAGE_BYTES.to_vec()),
    ]
}

pub fn fixture_bytes() -> Vec<u8> {
    build_zip(&members_with(&crlf(&source("contents.xml"))))
}

/// A temporary directory holding `sample.dsd`.
pub struct Work {
    pub dir: PathBuf,
}

impl Work {
    pub fn new(name: &str) -> Self {
        let dir = std::env::temp_dir().join(format!("dsd-test-{}-{name}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        std::fs::write(dir.join("sample.dsd"), fixture_bytes()).unwrap();
        Self { dir }
    }

    pub fn src(&self) -> PathBuf {
        self.dir.join("sample.dsd")
    }

    pub fn path(&self, name: &str) -> PathBuf {
        self.dir.join(name)
    }

    /// The fixture's contents.xml with CRLF line endings, as text.
    pub fn contents(&self) -> String {
        String::from_utf8(crlf(&source("contents.xml"))).unwrap()
    }

    pub fn variant(&self, name: &str, xml: &str) -> PathBuf {
        let path = self.path(name);
        std::fs::write(&path, build_zip(&members_with(xml.as_bytes()))).unwrap();
        path
    }

    pub fn zip(&self, name: &str, members: &[(Vec<u8>, Vec<u8>)]) -> PathBuf {
        let path = self.path(name);
        std::fs::write(&path, build_zip(members)).unwrap();
        path
    }
}

impl Drop for Work {
    fn drop(&mut self) {
        let _ = std::fs::remove_dir_all(&self.dir);
    }
}

/// Replace exactly one occurrence.
pub fn replace_once(text: &str, old: &str, new: &str) -> String {
    assert_eq!(text.matches(old).count(), 1, "expected one {old:?}");
    text.replacen(old, new, 1)
}

pub struct Run {
    pub code: i32,
    pub stdout: String,
    pub stderr: String,
}

impl Run {
    pub fn json(&self) -> Value {
        serde_json::from_str(&self.stdout).unwrap_or_else(|e| panic!("stdout is not JSON ({e}): {}", self.stdout))
    }

    pub fn ok(&self) -> Value {
        let v = self.json();
        assert_eq!(self.code, 0, "{}", self.stdout);
        assert_eq!(v["ok"], true);
        v["result"].clone()
    }

    pub fn fails(&self, code: &str) -> Value {
        let v = self.json();
        assert_eq!(self.code, if code == "invalid_invocation" { 2 } else { 1 }, "{}", self.stdout);
        assert_eq!(v["ok"], false);
        assert_eq!(v["error"]["code"], code, "{}", self.stdout);
        v["error"].clone()
    }

    /// The error message and hint, joined for substring checks.
    pub fn error_text(&self) -> String {
        let e = self.json()["error"].clone();
        format!("{} {}", e["message"].as_str().unwrap_or(""), e["hint"].as_str().unwrap_or(""))
    }
}

pub fn dsd<I, S>(args: I) -> Run
where
    I: IntoIterator<Item = S>,
    S: AsRef<std::ffi::OsStr>,
{
    let out = Command::new(env!("CARGO_BIN_EXE_dsd")).args(args).env("DSD_NO_UPDATE_CHECK", "1").output().unwrap();
    Run {
        code: out.status.code().unwrap_or(-1),
        stdout: String::from_utf8(out.stdout).unwrap(),
        stderr: String::from_utf8(out.stderr).unwrap(),
    }
}

/// Unpacked `contents.xml` of a written archive.
pub fn contents_of(path: &Path) -> String {
    let arc = dsd::read_archive(path).unwrap();
    String::from_utf8(arc.contents().unwrap().to_vec()).unwrap()
}
