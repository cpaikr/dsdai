#!/usr/bin/env python3
"""Read, check, and surgically edit DART Editor .dsd documents.

A .dsd is a ZIP archive holding contents.xml (the document), meta.xml (editor
metadata and image inventory), and any embedded images. This helper never
re-serializes XML: edits replace exact byte ranges located with expat, and
the archive is rewritten with the original member order, raw (often CP949)
member-name bytes, flags, and timestamps. Unchanged members are copied as
their original compressed bytes.

Standard library only; Python 3.9+. Run `dsd.py <command> --help` for usage.
"""

from __future__ import annotations

import argparse
import bisect
import difflib
import json
import os
import re
import struct
import sys
import tempfile
import xml.etree.ElementTree as ET
import xml.parsers.expat
import zipfile
import zlib
from dataclasses import dataclass, field
from pathlib import Path
from typing import Dict, Iterable, Iterator, List, Optional, Sequence, Tuple, Union

VERSION = "2.0.0"

CONTENTS = "contents.xml"
META = "meta.xml"
UTF8_BOM = b"\xef\xbb\xbf"
CR_TOKEN = "&cr;"  # DART Editor's in-text line break (stored as &amp;cr; in XML)
CELL_TAGS = ("TH", "TD", "TE", "TU")
ROW_SECTIONS = ("THEAD", "TBODY", "TFOOT")
SCAFFOLD_TAGS = ("COMMENT", "LIBRARYLIST", "FILENAME")  # editor-only, never printed
BLOCK_TEXT_TAGS = ("P", "TITLE", "COVER-TITLE", "IMG-CAPTION", "WARNING", "SUBTITLE")
# Elements the helper's rules are verified for (listed, with the unverified
# remainder, in references/format.md). Anything else is copied through byte for
# byte, but the helper does not guess at its meaning: check/inspect warn,
# read/outline/table/find mark it, and the edit commands refuse to touch it,
# leaving such edits to the extract -> text edit -> pack route.
#
# OBSERVED_TAGS were seen in real DART Editor output. Add one only after
# verifying rendering and edits on a real document that uses it.
OBSERVED_TAGS = frozenset({
    "A", "APPENDIX", "BODY", "COL", "COLGROUP", "COMMENT", "COMPANY-NAME", "COVER", "COVER-TITLE",
    "DOCUMENT", "DOCUMENT-HEADER", "DOCUMENT-NAME", "EXTRACTION", "FILENAME", "FORMULA-VERSION",
    "IMAGE", "IMG", "IMG-CAPTION", "INSERTION", "LIBRARY", "LIBRARYLIST", "P", "PART", "PGBRK",
    "SECTION-1", "SECTION-2", "SECTION-3", "SPAN", "SUMMARY", "TABLE", "TABLE-GROUP", "TBODY",
    "TD", "TE", "TH", "THEAD", "TITLE", "TOC", "TR", "TU", "WARNING",
})
# Never seen, but both DART schemas give them exactly the content model and
# attributes of an observed element, which the helper already handles by the
# same rule: SECTION-4..6 match SECTION-2, SECTION-7 matches it without a
# nested section (headings follow SECTION-<n>), and TFOOT matches TBODY
# (rows come from ROW_SECTIONS). A private-corpus test re-checks this against
# the installed schemas.
SCHEMA_EQUIVALENT_TAGS = frozenset({"SECTION-4", "SECTION-5", "SECTION-6", "SECTION-7", "TFOOT"})
VERIFIED_TAGS = OBSERVED_TAGS | SCHEMA_EQUIVALENT_TAGS
OBSERVED_SCHEMAS = ("dart3.xsd", "dart4.xsd")
RAW_ROUTE = "edit contents.xml as text instead (extract -> edit -> pack, then check --against and diff)"
MAX_ARCHIVE_BYTES = 200 * 1024 * 1024
MAX_MEMBER_BYTES = 200 * 1024 * 1024
MAX_MEMBERS = 2000

OPEN_TAG_RE = re.compile(
    rb"<[^\s/>]+(?:\s+[^\s=/>]+\s*=\s*(?:\"[^\"]*\"|'[^']*'))*\s*/?>"
)


class DsdError(Exception):
    """A failure the user can act on; printed without a traceback."""


# --------------------------------------------------------------------------
# Archive layer


@dataclass
class Member:
    info: zipfile.ZipInfo
    raw_name: bytes
    name: str  # decoded display name
    name_encoding: str
    data: bytes


@dataclass
class Archive:
    path: Path
    members: List[Member]
    comment: bytes = b""

    def get(self, name: str) -> Member:
        for m in self.members:
            if m.name == name:
                return m
        raise DsdError(f"{self.path.name}: missing required member {name!r}")

    @property
    def contents(self) -> bytes:
        return self.get(CONTENTS).data

    @property
    def meta(self) -> bytes:
        return self.get(META).data

    def images(self) -> List[Member]:
        return [m for m in self.members if m.name not in (CONTENTS, META)]


def _raw_name(info: zipfile.ZipInfo) -> bytes:
    if info.flag_bits & 0x800:
        return info.orig_filename.encode("utf-8")
    return info.orig_filename.encode("cp437")


def _decode_name(raw: bytes, utf8_flag: bool) -> Tuple[str, str]:
    if utf8_flag:
        return raw.decode("utf-8"), "utf-8"
    try:
        return raw.decode("ascii"), "ascii"
    except UnicodeDecodeError:
        pass
    try:
        return raw.decode("cp949"), "cp949"
    except UnicodeDecodeError:
        return raw.decode("cp437"), "cp437"


def read_archive(path: Union[str, Path]) -> Archive:
    path = Path(path)
    if not path.is_file():
        raise DsdError(f"{path}: file not found")
    if path.stat().st_size > MAX_ARCHIVE_BYTES:
        raise DsdError(f"{path.name}: archive exceeds {MAX_ARCHIVE_BYTES} bytes")
    try:
        zf = zipfile.ZipFile(path)
    except zipfile.BadZipFile as exc:
        raise DsdError(f"{path.name}: not a ZIP-based .dsd ({exc})") from None
    members: List[Member] = []
    seen = set()
    with zf:
        comment = zf.comment
        infos = zf.infolist()
        if len(infos) > MAX_MEMBERS:
            raise DsdError(f"{path.name}: too many members ({len(infos)})")
        for info in infos:
            raw = _raw_name(info)
            name, enc = _decode_name(raw, bool(info.flag_bits & 0x800))
            if info.flag_bits & 0x1:
                raise DsdError(f"{path.name}: encrypted member {name!r} is not supported")
            if (
                name.startswith(("/", "\\"))
                or ".." in re.split(r"[\\/]", name)
                or re.match(r"^[A-Za-z]:", name)
                or "\x00" in name
            ):
                raise DsdError(f"{path.name}: unsafe member path {name!r}")
            if name.lower() in seen:
                raise DsdError(f"{path.name}: duplicate member {name!r}")
            seen.add(name.lower())
            if info.file_size > MAX_MEMBER_BYTES:
                raise DsdError(f"{path.name}: member {name!r} is too large")
            if info.compress_type not in (zipfile.ZIP_STORED, zipfile.ZIP_DEFLATED):
                raise DsdError(
                    f"{path.name}: member {name!r} uses unsupported compression {info.compress_type}"
                )
            try:
                data = zf.read(info)
            except (zipfile.BadZipFile, zlib.error) as exc:
                raise DsdError(f"{path.name}: member {name!r} is corrupt ({exc})") from None
            members.append(Member(info, raw, name, enc, data))
    return Archive(path, members, comment)


def _raw_member(path: Path, info: zipfile.ZipInfo) -> Tuple[bytes, bytes]:
    """Return (local-header extra field, compressed payload) as stored."""
    with open(path, "rb") as fh:
        fh.seek(info.header_offset)
        header = fh.read(30)
        if header[:4] != b"PK\x03\x04":
            raise DsdError(f"{path.name}: bad local header for {info.filename!r}")
        name_len, extra_len = struct.unpack("<HH", header[26:30])
        fh.seek(info.header_offset + 30 + name_len)
        extra = fh.read(extra_len)
        return extra, fh.read(info.compress_size)


def _without_zip64(extra: bytes) -> bytes:
    """Drop ZIP64 (0x0001) records; this writer emits classic headers only."""
    out, pos = [], 0
    while pos + 4 <= len(extra):
        tag, size = struct.unpack("<HH", extra[pos : pos + 4])
        if tag != 0x0001:
            out.append(extra[pos : pos + 4 + size])
        pos += 4 + size
    return b"".join(out)


def _dos_datetime(dt: Tuple[int, int, int, int, int, int]) -> Tuple[int, int]:
    y, mo, d, h, mi, s = dt
    return ((h << 11) | (mi << 5) | (s // 2)), (((max(y, 1980) - 1980) << 9) | (mo << 5) | d)


def write_archive(
    source: Archive,
    replacements: Dict[str, bytes],
    out_path: Path,
    force: bool = False,
) -> None:
    """Write `source` to `out_path`, swapping member data named in `replacements`.

    Member order, raw name bytes, general-purpose flags (minus data
    descriptors), timestamps, attributes, extra fields (minus ZIP64), and
    comments are preserved. Unchanged members keep their original compressed
    bytes.
    """
    unknown = set(replacements) - {m.name for m in source.members}
    if unknown:
        raise DsdError(f"cannot replace missing members: {sorted(unknown)}")
    _guard_output(out_path, force, inputs=[source.path])
    local_parts: List[bytes] = []
    central: List[bytes] = []
    offset = 0
    for m in source.members:
        info = m.info
        local_extra, original_payload = _raw_member(source.path, info)
        local_extra, central_extra = _without_zip64(local_extra), _without_zip64(info.extra)
        if m.name in replacements and replacements[m.name] != m.data:
            data = replacements[m.name]
            method = info.compress_type
            if method == zipfile.ZIP_DEFLATED:
                comp = zlib.compressobj(6, zlib.DEFLATED, -15)
                payload = comp.compress(data) + comp.flush()
            else:
                payload = data
            crc = zlib.crc32(data) & 0xFFFFFFFF
            size = len(data)
        else:
            method = info.compress_type
            payload = original_payload
            crc = info.CRC
            size = info.file_size
        if size > 0xFFFFFFFF or len(payload) > 0xFFFFFFFF:
            raise DsdError("member too large for a non-ZIP64 archive")
        flags = info.flag_bits & ~0x08
        dos_time, dos_date = _dos_datetime(info.date_time)
        local = struct.pack(
            "<4sHHHHHIIIHH",
            b"PK\x03\x04",
            info.extract_version,
            flags,
            method,
            dos_time,
            dos_date,
            crc,
            len(payload),
            size,
            len(m.raw_name),
            len(local_extra),
        )
        central.append(
            struct.pack(
                "<4sHHHHHHIIIHHHHHII",
                b"PK\x01\x02",
                (info.create_system << 8) | info.create_version,
                info.extract_version,
                flags,
                method,
                dos_time,
                dos_date,
                crc,
                len(payload),
                size,
                len(m.raw_name),
                len(central_extra),
                len(info.comment),
                0,
                info.internal_attr,
                info.external_attr,
                offset,
            )
            + m.raw_name
            + central_extra
            + info.comment
        )
        part = local + m.raw_name + local_extra + payload
        local_parts.append(part)
        offset += len(part)
    cd = b"".join(central)
    eocd = struct.pack(
        "<4sHHHHIIH",
        b"PK\x05\x06",
        0,
        0,
        len(central),
        len(central),
        len(cd),
        offset,
        len(source.comment),
    )
    _atomic_write(out_path, b"".join(local_parts) + cd + eocd + source.comment)


def _same_file(a: Path, b: Path) -> bool:
    try:
        return a.resolve() == b.resolve()
    except OSError:
        return False


def _guard_output(out_path: Path, force: bool, inputs: Sequence[Path] = ()) -> None:
    if out_path.exists() and not force:
        raise DsdError(f"{out_path}: already exists; pass --force only if replacing it is authorized")
    for src in inputs:
        if _same_file(out_path, src) and not force:
            raise DsdError(f"{out_path}: refusing to overwrite the input in place without --force")


def _atomic_write(out_path: Path, data: bytes) -> None:
    out_path.parent.mkdir(parents=True, exist_ok=True)
    fd, tmp = tempfile.mkstemp(prefix=".dsd-", dir=str(out_path.parent))
    try:
        with os.fdopen(fd, "wb") as fh:
            fh.write(data)
            fh.flush()
            os.fsync(fh.fileno())
        os.replace(tmp, out_path)
    except BaseException:
        if os.path.exists(tmp):
            os.unlink(tmp)
        raise


# --------------------------------------------------------------------------
# Positioned XML model


@dataclass(eq=False)
class Node:
    tag: str
    attrs: Dict[str, str]
    start: int  # offset of '<'
    open_end: int  # offset just after the start tag's '>'
    close_start: int = -1  # offset of '</' (== open_end for empty-element tags)
    end: int = -1  # offset just after the element
    parent: Optional["Node"] = None
    items: List[Union[str, "Node"]] = field(default_factory=list)
    empty_tag: bool = False  # written as <X ... /> rather than <X ...></X>

    @property
    def children(self) -> List["Node"]:
        return [i for i in self.items if isinstance(i, Node)]

    def iter(self, tag: Optional[str] = None) -> Iterator["Node"]:
        if tag is None or self.tag == tag:
            yield self
        for child in self.children:
            yield from child.iter(tag)

    def ancestors(self) -> Iterator["Node"]:
        node = self.parent
        while node is not None:
            yield node
            node = node.parent


@dataclass
class Cell:
    node: Node
    row: int  # 1-based origin row
    col: int  # 1-based origin column
    rowspan: int
    colspan: int


@dataclass
class Table:
    index: int  # 1-based document order
    node: Node
    cols: List[int]
    rows: List[Node]
    grid: List[List[Optional[Cell]]]
    cells: List[Cell]
    issues: List[str]

    @property
    def ncols(self) -> int:
        return len(self.cols)


class Doc:
    def __init__(self, data: bytes):
        self.data = data
        self.root = _parse_positioned(data)
        self._newlines = [m.start() for m in re.finditer(b"\n", data)]
        self.tables = [build_table(i + 1, n) for i, n in enumerate(self.root.iter("TABLE"))]

    def line(self, offset: int) -> int:
        return bisect.bisect_right(self._newlines, offset - 1) + 1

    def table(self, index: int) -> Table:
        if not 1 <= index <= len(self.tables):
            raise DsdError(f"table {index} does not exist (document has {len(self.tables)} tables)")
        return self.tables[index - 1]

    def newline(self) -> bytes:
        return b"\r\n" if b"\r\n" in self.data[:4096] else b"\n"


def _parse_positioned(data: bytes) -> Node:
    parser = xml.parsers.expat.ParserCreate()
    stack: List[Node] = []
    holder: List[Node] = []

    def start(name: str, attrs: Dict[str, str]) -> None:
        pos = parser.CurrentByteIndex
        m = OPEN_TAG_RE.match(data, pos)
        if not m:
            raise DsdError(f"cannot locate start tag <{name}> at byte {pos}")
        node = Node(name, attrs, pos, m.end(), parent=stack[-1] if stack else None)
        if m.group(0).endswith(b"/>"):
            node.close_start = node.end = m.end()
            node.empty_tag = True
        if stack:
            stack[-1].items.append(node)
        else:
            holder.append(node)
        stack.append(node)

    def end(name: str) -> None:
        node = stack.pop()
        if node.end < 0:
            pos = parser.CurrentByteIndex
            node.close_start = pos
            node.end = data.index(b">", pos) + 1

    def chars(text: str) -> None:
        items = stack[-1].items
        if items and isinstance(items[-1], str):
            items[-1] += text
        else:
            items.append(text)

    def refuse_doctype(*_args: object) -> None:
        raise DsdError("DOCTYPE/entity declarations are not allowed in .dsd XML")

    parser.StartElementHandler = start
    parser.EndElementHandler = end
    parser.CharacterDataHandler = chars
    parser.StartDoctypeDeclHandler = refuse_doctype
    parser.EntityDeclHandler = refuse_doctype
    try:
        parser.Parse(data, True)
    except xml.parsers.expat.ExpatError as exc:
        raise DsdError(f"XML is not well-formed: {exc}") from None
    if not holder:
        raise DsdError("XML has no root element")
    return holder[0]


def _int_attr(node: Node, name: str, default: int = 1) -> int:
    try:
        return max(1, int(node.attrs.get(name, default)))
    except ValueError:
        return default


def build_table(index: int, node: Node) -> Table:
    cols: List[int] = []
    rows: List[Node] = []
    for child in node.children:
        if child.tag == "COLGROUP":
            for col in child.children:
                if col.tag == "COL":
                    try:
                        cols.append(int(col.attrs.get("WIDTH", "0")))
                    except ValueError:
                        cols.append(0)
        elif child.tag in ROW_SECTIONS:
            rows.extend(r for r in child.children if r.tag == "TR")
        elif child.tag == "TR":
            rows.append(child)
    issues: List[str] = []
    if not cols and rows:
        # No COLGROUP: infer the grid width from the widest row (sizes unknown).
        widths = [sum(_int_attr(c, "COLSPAN") for c in r.children if c.tag in CELL_TAGS) for r in rows]
        cols = [0] * max(widths)
    ncols = len(cols)
    grid: List[List[Optional[Cell]]] = [[None] * max(ncols, 1) for _ in rows]
    cells: List[Cell] = []
    for ri, row in enumerate(rows):
        ci = 0
        for cnode in row.children:
            if cnode.tag not in CELL_TAGS:
                continue
            while ci < ncols and grid[ri][ci] is not None:
                ci += 1
            rs, cs = _int_attr(cnode, "ROWSPAN"), _int_attr(cnode, "COLSPAN")
            cell = Cell(cnode, ri + 1, ci + 1, rs, cs)
            cells.append(cell)
            if ci + cs > ncols:
                issues.append(f"R{ri + 1}C{ci + 1}: cell spans past the {ncols} declared columns")
            if ri + rs > len(rows):
                issues.append(f"R{ri + 1}C{ci + 1}: ROWSPAN {rs} runs past the last row")
            for a in range(rs):
                for b in range(cs):
                    if ri + a < len(rows) and ci + b < ncols:
                        if grid[ri + a][ci + b] is not None:
                            issues.append(f"R{ri + a + 1}C{ci + b + 1}: overlapping cells")
                        grid[ri + a][ci + b] = cell
            ci += cs
    for ri, row in enumerate(grid):
        missing = [str(ci + 1) for ci in range(ncols) if row[ci] is None]
        if missing:
            issues.append(f"R{ri + 1}: no cell covers column(s) {','.join(missing)}")
    return Table(index, node, cols, rows, grid, cells, issues)


def cell_at(table: Table, row: int, col: int) -> Cell:
    if not 1 <= row <= len(table.rows):
        raise DsdError(f"T{table.index} has rows 1..{len(table.rows)}; R{row} does not exist")
    if not 1 <= col <= table.ncols:
        raise DsdError(f"T{table.index} has columns 1..{table.ncols}; C{col} does not exist")
    cell = table.grid[row - 1][col - 1]
    if cell is None:
        raise DsdError(f"T{table.index} R{row}C{col} is not covered by any cell")
    return cell


# --------------------------------------------------------------------------
# Text extraction


def node_text(node: Node, include_scaffold: bool = False) -> str:
    """Plain text with DART line breaks (&cr;) and block boundaries as '\n'.

    Literal newlines in the XML source are layout, not content, so they are
    dropped; DART stores real line breaks as the &cr; token.
    """
    boundary = "\x1e"
    parts: List[str] = []
    for item in node.items:
        if isinstance(item, str):
            parts.append(item.replace("\r", "").replace("\n", ""))
        elif item.tag in SCAFFOLD_TAGS and not include_scaffold:
            continue
        elif item.tag in BLOCK_TEXT_TAGS:
            parts.append(boundary + node_text(item, include_scaffold) + boundary)
        else:
            parts.append(node_text(item, include_scaffold))
    # Adjacent blocks are separated by one newline; &cr; runs keep their blank lines.
    segments = "".join(parts).replace(CR_TOKEN, "\n").split(boundary)
    text = "\n".join(s for s in segments if s != "" and s.strip(" \t　") != "")
    lines = [ln.rstrip() for ln in text.split("\n")]
    while lines and not lines[0].strip():
        lines.pop(0)
    while lines and not lines[-1].strip():
        lines.pop()
    return "\n".join(lines)


def compact(text: str) -> str:
    return re.sub(r"\s+", "", text)


@dataclass
class Block:
    locator: str
    tag: str
    text: str
    line: int
    attrs: Tuple[Tuple[str, str], ...] = ()
    unverified: Tuple[str, ...] = ()  # unverified tags in or around this block
    group: str = ""  # cells of one table row share a group ("T3R4"); a text block is its own group

    def attr_text(self) -> str:
        return " ".join(f'{k}="{v}"' for k, v in self.attrs)

    def key(self) -> Tuple[str, str, Tuple[Tuple[str, str], ...]]:
        return self.tag, self.text, self.attrs


def iter_blocks(doc: Doc) -> Iterator[Block]:
    """Readable units in document order: table cells and free text blocks."""
    table_by_node = {t.node: t for t in doc.tables}

    def walk(node: Node, outer: Tuple[str, ...]) -> Iterator[Block]:
        for child in node.children:
            if child.tag in SCAFFOLD_TAGS or child.tag == "DOCUMENT-HEADER":
                continue
            if child.tag == "TABLE":
                table = table_by_node[child]
                around = tuple(sorted(set(outer) | set(unverified_tags(child))))
                for cell in table.cells:
                    yield Block(
                        f"T{table.index}R{cell.row}C{cell.col}",
                        cell.node.tag,
                        node_text(cell.node),
                        doc.line(cell.node.start),
                        tuple(sorted(cell.node.attrs.items())),
                        around,
                        f"T{table.index}R{cell.row}",
                    )
            elif child.tag in BLOCK_TEXT_TAGS:
                yield Block(
                    f"L{doc.line(child.start)}",
                    child.tag,
                    node_text(child),
                    doc.line(child.start),
                    tuple(sorted(child.attrs.items())),
                    tuple(sorted(set(outer) | set(unverified_tags(child)))),
                    f"@{child.start}",  # byte offset: unique even when blocks share a line
                )
            else:
                inner = outer if child.tag in VERIFIED_TAGS else tuple(sorted(set(outer) | {child.tag}))
                yield from walk(child, inner)

    yield from walk(doc.root, ())


# --------------------------------------------------------------------------
# Metadata and checks


@dataclass
class Issue:
    level: str  # error | warning | info
    code: str
    message: str

    def as_dict(self) -> Dict[str, str]:
        return {"level": self.level, "code": self.code, "message": self.message}


def parse_meta(data: bytes) -> ET.Element:
    if b"<!DOCTYPE" in data or b"<!ENTITY" in data:
        raise DsdError("meta.xml contains DOCTYPE/entity declarations")
    try:
        return ET.fromstring(data)
    except ET.ParseError as exc:
        raise DsdError(f"meta.xml is not well-formed: {exc}") from None


def header_info(doc: Doc) -> Dict[str, str]:
    info: Dict[str, str] = {}
    schema = ""
    for k, v in doc.root.attrs.items():
        if k.endswith("noNamespaceSchemaLocation"):
            schema = v
    info["schema"] = schema
    for node in doc.root.iter("DOCUMENT-NAME"):
        info["document_name"] = node_text(node)
        info["document_code"] = node.attrs.get("ACODE", "")
        break
    for node in doc.root.iter("FORMULA-VERSION"):
        info["formula_version"] = node_text(node)
        info["formula_date"] = node.attrs.get("ADATE", "")
        break
    return info


def img_refs(doc: Doc) -> List[str]:
    return [node_text(n) for n in doc.root.iter("IMG")]


def unverified_nodes(node: Node) -> Iterator[Node]:
    """Elements in `node`'s subtree (itself included) outside VERIFIED_TAGS.

    Editor scaffolding (COMMENT, LIBRARYLIST, FILENAME) is skipped: the helper
    never renders or edits its contents, so nothing inside it is interpreted.
    """
    if node.tag in SCAFFOLD_TAGS:
        return
    if node.tag not in VERIFIED_TAGS:
        yield node
    for child in node.children:
        yield from unverified_nodes(child)


def unverified_tags(node: Node) -> List[str]:
    """Tags of unverified_nodes(node), sorted and unique."""
    return sorted({n.tag for n in unverified_nodes(node)})


def unverified_ancestor_tags(node: Node) -> List[str]:
    return sorted({a.tag for a in node.ancestors() if a.tag not in VERIFIED_TAGS})


def unverified_schema(doc: Doc) -> Optional[str]:
    """The declared schema when it is not one the helper was verified on."""
    schema = header_info(doc)["schema"]
    return None if schema.lower() in OBSERVED_SCHEMAS else (schema or "(none declared)")


def _require_verified(doc: Doc, node: Node, where: str) -> None:
    """Refuse an edit whose interpretation depends on unverified structure."""
    schema = unverified_schema(doc)
    if schema:
        raise DsdError(f"document schema {schema} is not one this helper was verified on; {RAW_ROUTE}")
    tags = unverified_tags(node)
    if tags:
        names = ", ".join(f"<{t}>" for t in tags)
        raise DsdError(f"{where} contains {names}, which this helper has not verified; {RAW_ROUTE}")
    # An unverified container may change what the target means (as replace and
    # find already assume), so its ancestors count too.
    outer = unverified_ancestor_tags(node)
    if outer:
        names = ", ".join(f"<{t}>" for t in outer)
        raise DsdError(f"{where} lies inside {names}, which this helper has not verified; {RAW_ROUTE}")


def check_archive(arc: Archive) -> Tuple[List[Issue], Optional[Doc]]:
    issues: List[Issue] = []
    names = [m.name for m in arc.members]
    for required in (CONTENTS, META):
        if required not in names:
            issues.append(Issue("error", "missing-member", f"{required} is missing"))
    if any(i.level == "error" for i in issues):
        return issues, None
    for m in arc.members:
        if m.name_encoding == "cp437":
            issues.append(Issue("warning", "name-encoding", f"member name not decodable as CP949: {m.name!r}"))
    try:
        arc.contents.decode("utf-8")
    except UnicodeDecodeError as exc:
        issues.append(Issue("error", "contents-encoding", f"contents.xml is not valid UTF-8: {exc}"))
        return issues, None
    try:
        doc = Doc(arc.contents)
    except DsdError as exc:
        issues.append(Issue("error", "contents-xml", f"contents.xml: {exc}"))
        return issues, None
    if doc.root.tag != "DOCUMENT":
        issues.append(Issue("error", "root", f"contents.xml root is <{doc.root.tag}>, expected <DOCUMENT>"))
    try:
        meta = parse_meta(arc.meta)
    except DsdError as exc:
        issues.append(Issue("error", "meta-xml", str(exc)))
        meta = None
    head = header_info(doc)
    schema = unverified_schema(doc)
    if schema:
        issues.append(Issue(
            "warning", "unverified-schema",
            f"schema {schema} is not one this helper was verified on ({', '.join(OBSERVED_SCHEMAS)});"
            " edit commands refuse this file",
        ))
    unseen: Dict[str, List[Node]] = {}
    for node in unverified_nodes(doc.root):
        unseen.setdefault(node.tag, []).append(node)
    for tag, nodes in sorted(unseen.items()):
        issues.append(Issue(
            "warning", "unverified-element",
            f"<{tag}> x{len(nodes)}, first at line {doc.line(nodes[0].start)}: not seen in verified DART output;"
            " readers mark it and edit commands refuse to touch it",
        ))
    if meta is not None:
        gen = meta.find("GENERATOR")
        meta_schema = gen.get("schema", "") if gen is not None else ""
        if meta_schema and head["schema"] and meta_schema.lower() != head["schema"].lower():
            issues.append(
                Issue("warning", "schema-mismatch", f"meta.xml schema {meta_schema} != contents.xml {head['schema']}")
            )
        meta_imgs = [e.get("filename", "") for e in meta.iter("IMG")]
    else:
        meta_imgs = []
    member_imgs = sorted(m.name for m in arc.images())
    refs = img_refs(doc)
    for ref in sorted(set(refs) - set(member_imgs)):
        issues.append(Issue("error", "image-missing", f"contents.xml references image not in archive: {ref!r}"))
    if meta is not None:
        for ref in sorted(set(meta_imgs) - set(member_imgs)):
            issues.append(Issue("error", "image-missing", f"meta.xml lists image not in archive: {ref!r}"))
        for name in sorted(set(member_imgs) - set(meta_imgs)):
            issues.append(Issue("warning", "image-unlisted", f"archive image missing from meta.xml list: {name!r}"))
    for name in sorted(set(member_imgs) - set(refs)):
        issues.append(Issue("warning", "image-unreferenced", f"archive image not referenced in contents.xml: {name!r}"))

    data = doc.data
    crlf = data.count(b"\r\n")
    lf = data.count(b"\n")
    if crlf and lf != crlf:
        issues.append(Issue("warning", "line-endings", f"mixed line endings ({lf - crlf} bare LF among {crlf} CRLF)"))

    toc_ids: List[int] = []
    for node in doc.root.iter():
        if node.tag != "BODY" and "ATOCID" in node.attrs:
            try:
                toc_ids.append(int(node.attrs["ATOCID"]))
            except ValueError:
                issues.append(Issue("warning", "toc-id", f"line {doc.line(node.start)}: non-numeric ATOCID"))
    dupes = sorted({i for i in toc_ids if toc_ids.count(i) > 1})
    if dupes:
        issues.append(Issue("warning", "toc-id", f"duplicate ATOCID values: {dupes[:10]}"))
    body = next(doc.root.iter("BODY"), None)
    if body is not None and toc_ids and body.attrs.get("ATOCID", "").isdigit():
        if int(body.attrs["ATOCID"]) != max(toc_ids):
            issues.append(
                Issue(
                    "warning",
                    "toc-id",
                    f"BODY ATOCID={body.attrs['ATOCID']} but highest descendant ATOCID is {max(toc_ids)}",
                )
            )

    for table in doc.tables:
        where = f"T{table.index} (line {doc.line(table.node.start)})"
        for problem in table.issues:
            issues.append(Issue("error", "table-grid", f"{where} {problem}"))
        issues.extend(_width_issues(doc, table, where))
        for cell in table.cells:
            issues.extend(_unit_issues(doc, table, cell))
    issues.extend(_period_issues(doc))
    return issues, doc


def _width_issues(doc: Doc, table: Table, where: str) -> List[Issue]:
    """Compare cell WIDTH/HEIGHT with the table's own sizing convention.

    Observed conventions: Dart4-era editors write cell WIDTH = sum(spanned COL)
    - 9; an older Dart3-era editor wrote cell WIDTH = sum(spanned COL) + 9 x
    COLSPAN. Cell HEIGHT = sum(spanned TR HEIGHT) - 7 in both.
    """
    if not any(table.cols) or not table.cells:
        return []
    deltas: Dict[str, int] = {"modern": 0, "legacy": 0}
    measured: List[Tuple[Cell, int, int]] = []
    for cell in table.cells:
        try:
            width = int(cell.node.attrs.get("WIDTH", ""))
        except ValueError:
            continue
        span = sum(table.cols[cell.col - 1 : cell.col - 1 + cell.colspan])
        modern, legacy = span - 9, span + 9 * cell.colspan
        measured.append((cell, width - modern, width - legacy))
        deltas["modern"] += width == modern
        deltas["legacy"] += width == legacy
    convention = "modern" if deltas["modern"] >= deltas["legacy"] else "legacy"
    out: List[Issue] = []
    off = [c for c, dm, dl in measured if (dm if convention == "modern" else dl) != 0]
    if off and len(off) < len(measured):
        sample = ", ".join(f"R{c.row}C{c.col}" for c in off[:5])
        out.append(
            Issue("warning", "cell-width", f"{where}: {len(off)} cell WIDTH value(s) differ from the table's convention ({sample})")
        )
    heights: List[str] = []
    for cell in table.cells:
        try:
            h = int(cell.node.attrs.get("HEIGHT", ""))
            rows = table.rows[cell.row - 1 : cell.row - 1 + cell.rowspan]
            expected = sum(int(r.attrs["HEIGHT"]) for r in rows) - 7
        except (ValueError, KeyError):
            continue
        if h != expected:
            heights.append(f"R{cell.row}C{cell.col}")
    if heights:
        out.append(
            Issue("warning", "cell-height", f"{where}: cell HEIGHT differs from row HEIGHT - 7 at {', '.join(heights[:5])}")
        )
    return out


DATE_UNITS_RE = re.compile(r"(DT|DATE|PERIODFROM|PERIODTO)")


def _unit_issues(doc: Doc, table: Table, cell: Cell) -> List[Issue]:
    node = cell.node
    if node.tag != "TU":
        return []
    value = node.attrs.get("AUNITVALUE", "")
    unit = node.attrs.get("AUNIT", "")
    if not value or not DATE_UNITS_RE.search(unit) or not re.fullmatch(r"\d{8}", value):
        return []
    shown = _date_digits(node_text(node))
    if not shown or shown == value:
        return []
    where = f"T{table.index}R{cell.row}C{cell.col}"
    return [Issue("warning", "unit-value", f"{where}: TU {unit} text shows {shown} but AUNITVALUE={value}")]


PERIOD_UNIT_RE = re.compile(r"(PERIODFROM|PERIODTO)2?")


def _period_issues(doc: Doc) -> List[Issue]:
    """Warn when TU cells for the same reporting-period bound disagree.

    Observed: forms that repeat the period (cover and attached-statements
    title) write the repeat as PERIODFROM2/PERIODTO2 with the same machine
    date as PERIODFROM/PERIODTO, so a lone edit to one copy leaves the other
    stale. Other unit kinds (e.g. SUB_PERIODFROM) are not compared.
    """
    bounds: Dict[str, List[Tuple[str, str]]] = {}
    for table in doc.tables:
        for cell in table.cells:
            node = cell.node
            m = PERIOD_UNIT_RE.fullmatch(node.attrs.get("AUNIT", ""))
            if node.tag == "TU" and m and node.attrs.get("AUNITVALUE"):
                loc = f"T{table.index}R{cell.row}C{cell.col}"
                bounds.setdefault(m.group(1), []).append((loc, node.attrs["AUNITVALUE"]))
    out: List[Issue] = []
    for bound, cells in sorted(bounds.items()):
        if len({value for _, value in cells}) > 1:
            listing = ", ".join(f"{loc}={value}" for loc, value in cells)
            out.append(Issue("warning", "period-mismatch", f"{bound}/{bound}2 TU cells disagree: {listing}"))
    return out


def schema_validate(contents: bytes, schema_dir: Path, schema_name: str) -> List[Issue]:
    try:
        from lxml import etree  # type: ignore
    except ImportError:
        return [Issue("info", "schema-skipped", "lxml is not installed; schema validation skipped")]
    candidates = [p for p in schema_dir.iterdir() if p.name.lower() == schema_name.lower()]
    if not candidates:
        return [Issue("warning", "schema-missing", f"{schema_name} not found in {schema_dir}")]
    schema = etree.XMLSchema(etree.parse(str(candidates[0])))
    parser = etree.XMLParser(resolve_entities=False, no_network=True, load_dtd=False)
    tree = etree.fromstring(contents, parser)
    if schema.validate(tree):
        return [Issue("info", "schema-valid", f"contents.xml validates against {candidates[0].name}")]
    errs = list(schema.error_log)
    out = [Issue("error", "schema-invalid", f"line {e.line}: {e.message}") for e in errs[:20]]
    if len(errs) > 20:
        out.append(Issue("error", "schema-invalid", f"... {len(errs) - 20} more schema errors"))
    return out


def default_schema_dir() -> Optional[Path]:
    env = os.environ.get("DART_SCHEMA_DIR", "")
    for candidate in ([Path(env)] if env else []) + [Path("C:/DART/Resource/Dtd")]:
        if candidate.is_dir():
            return candidate
    return None


# --------------------------------------------------------------------------
# Editing primitives (byte-range replacements on contents.xml)


def xml_escape_text(text: str) -> str:
    text = text.replace("\r\n", "\n").replace("\r", "\n")
    escaped = text.replace("&", "&amp;").replace("<", "&lt;").replace(">", "&gt;").replace('"', "&quot;")
    return escaped.replace("\n", "&amp;cr;")


def _leaf_for_text(cell: Node) -> Optional[Node]:
    """Return the element whose inner content can be replaced by plain text.

    Descends through a single P or SPAN wrapper so paragraph and span
    formatting (USERMARK) survives; refuses mixed or multi-block content.
    """
    node = cell
    while True:
        kids = node.children
        loose_text = "".join(i for i in node.items if isinstance(i, str)).strip()
        if not kids:
            return node
        if len(kids) == 1 and not loose_text and kids[0].tag in ("P", "SPAN"):
            node = kids[0]
            continue
        return None


def _replace_ranges(data: bytes, edits: List[Tuple[int, int, bytes]]) -> bytes:
    edits = sorted(edits, key=lambda e: e[0])
    for (a1, b1, _), (a2, _, _) in zip(edits, edits[1:]):
        if a2 < b1:
            raise DsdError("internal error: overlapping edits")
    out: List[bytes] = []
    pos = 0
    for a, b, new in edits:
        out.append(data[pos:a])
        out.append(new)
        pos = b
    out.append(data[pos:])
    return b"".join(out)


def _set_attr(data: bytes, node: Node, name: str, value: str) -> Tuple[int, int, bytes]:
    tag = data[node.start : node.open_end]
    escaped = value.replace("&", "&amp;").replace('"', "&quot;").replace("<", "&lt;").encode("utf-8")
    pat = re.compile(rb'(\s' + re.escape(name.encode()) + rb'\s*=\s*")([^"]*)(")')
    m = pat.search(tag)
    if m:
        new_tag = tag[: m.start(2)] + escaped + tag[m.end(2) :]
    else:
        insert_at = len(tag) - (2 if tag.endswith(b"/>") else 1)
        new_tag = tag[:insert_at] + b" " + name.encode() + b'="' + escaped + b'"' + tag[insert_at:]
    return node.start, node.open_end, new_tag


def op_set_cell(
    data: bytes,
    table_no: int,
    row: int,
    col: int,
    text: str,
    unit_value: Optional[str] = None,
    flatten: bool = False,
) -> Tuple[bytes, str]:
    doc = Doc(data)
    table = doc.table(table_no)
    # Coordinates come from the whole table's structure, so check all of it.
    _require_verified(doc, table.node, f"T{table_no}")
    cell = cell_at(table, row, col)
    node = cell.node
    if (cell.row, cell.col) != (row, col):
        # Writing the anchor instead would silently change a header or label
        # the caller never addressed.
        raise DsdError(
            f"T{table_no} R{row}C{col} is covered by the spanning cell at R{cell.row}C{cell.col};"
            f" address that cell (--row {cell.row} --col {cell.col}) if it is the one to change"
        )
    where = f"T{table_no}R{cell.row}C{cell.col}"
    if node.tag == "TU" and node.attrs.get("AUNITVALUE") and unit_value is None:
        raise DsdError(
            f"{where} is a TU cell with AUNITVALUE={node.attrs['AUNITVALUE']!r}"
            f" (AUNIT={node.attrs.get('AUNIT', '')}); pass the matching machine value with --unit-value"
        )
    if unit_value is not None:
        if node.tag != "TU":
            raise DsdError(f"--unit-value applies only to TU cells; {where} is {node.tag}")
        shown = _date_digits(text)
        if DATE_UNITS_RE.search(node.attrs.get("AUNIT", "")) and re.fullmatch(r"\d{8}", unit_value) and shown and shown != unit_value:
            raise DsdError(f"{where}: text shows date {shown} but --unit-value is {unit_value}; make them agree")
    leaf = _leaf_for_text(node)
    if leaf is None:
        if not flatten:
            raise DsdError(
                f"{where} holds mixed or multi-paragraph content;"
                " edit contents.xml directly, or pass --flatten to replace it with plain text"
            )
        leaf = node
    new_inner = xml_escape_text(text).encode("utf-8")
    edits: List[Tuple[int, int, bytes]] = []
    if unit_value is not None and leaf is not node:
        edits.append(_set_attr(data, node, "AUNITVALUE", unit_value))
    if leaf.empty_tag:  # written as <P/> or <TD .../>
        open_tag = data[leaf.start : leaf.open_end - 2].rstrip() + b">"
        if unit_value is not None and leaf is node:
            open_tag = _set_attr(open_tag, Node(node.tag, {}, 0, len(open_tag)), "AUNITVALUE", unit_value)[2]
        edits.append((leaf.start, leaf.end, open_tag + new_inner + f"</{leaf.tag}>".encode()))
    else:
        if unit_value is not None and leaf is node:
            edits.append(_set_attr(data, node, "AUNITVALUE", unit_value))
        edits.append((leaf.open_end, leaf.close_start, new_inner))
    before = node_text(node)
    return _replace_ranges(data, edits), f"{where} {node.tag}: {before!r} -> {text!r}"


def _date_digits(text: str) -> str:
    digits = re.findall(r"\d+", text)
    if len(digits) >= 3 and len(digits[0]) == 4:
        return f"{int(digits[0]):04d}{int(digits[1]):02d}{int(digits[2]):02d}"
    return ""


NON_TEXT_RE = re.compile(rb"<!--.*?-->|<\?.*?\?>|<!\[CDATA\[.*?\]\]>", re.S)
TEXT_UNIT_RE = re.compile(rb"&amp;cr;|&[A-Za-z]+;|&#x?[0-9A-Fa-f]+;|[\x00-\x7f]|[\xc0-\xff][\x80-\xbf]*")
NAMED_ENTITIES = {"amp": "&", "lt": "<", "gt": ">", "quot": '"', "apos": "'"}


def _text_runs(node: Node, skip_scaffold: bool = True) -> Iterator[Tuple[int, int]]:
    """Byte ranges of character data directly inside elements of `node`.

    Ranges come from the parsed tree, so tags, attribute values, comments,
    processing instructions, and CDATA are never included. Editor scaffolding
    (COMMENT, LIBRARYLIST, FILENAME) is skipped unless asked otherwise.
    """
    if skip_scaffold and node.tag in SCAFFOLD_TAGS:
        return
    if node.close_start == node.open_end:
        return
    kids = node.children
    bounds = [node.open_end] + [b for k in kids for b in (k.start, k.end)] + [node.close_start]
    for a, b in zip(bounds[::2], bounds[1::2]):
        yield (a, b)
    for kid in kids:
        yield from _text_runs(kid, skip_scaffold)


def _decode_run(data: bytes, a: int, b: int) -> Tuple[str, List[Tuple[int, int]]]:
    """Decode raw character data, mapping each logical character to its bytes.

    `&amp;cr;` becomes a single '\n' (DART's line break); entity references
    become their character. Non-text constructs inside the run are skipped.
    """
    chars: List[str] = []
    spans: List[Tuple[int, int]] = []
    pos = a
    for m in NON_TEXT_RE.finditer(data, a, b):
        _decode_units(data, pos, m.start(), chars, spans)
        chars.append("\x00")  # barrier: matches never cross a comment
        spans.append((m.start(), m.end()))
        pos = m.end()
    _decode_units(data, pos, b, chars, spans)
    return "".join(chars), spans


def _decode_units(data: bytes, a: int, b: int, chars: List[str], spans: List[Tuple[int, int]]) -> None:
    for m in TEXT_UNIT_RE.finditer(data, a, b):
        unit = m.group(0)
        if unit == b"&amp;cr;":
            ch = "\n"
        elif unit.startswith(b"&#"):
            body = unit[2:-1].decode()
            ch = chr(int(body[1:], 16) if body[:1] in "xX" else int(body))
        elif unit.startswith(b"&") and len(unit) > 1:
            ch = NAMED_ENTITIES.get(unit[1:-1].decode(), unit.decode())
        else:
            ch = unit.decode("utf-8", "replace")
            if ch in "\r\n":
                ch = "\x00"  # XML layout newline, not content
        chars.append(ch)
        spans.append((m.start(), m.end()))


def find_text_hits(doc: Doc, old: str, root: Node) -> List[Tuple[int, int]]:
    needle = old.replace("\r\n", "\n").replace("\r", "\n")
    hits: List[Tuple[int, int]] = []
    for a, b in _text_runs(root):
        text, spans = _decode_run(doc.data, a, b)
        pos = 0
        while True:
            idx = text.find(needle, pos)
            if idx < 0:
                break
            hits.append((spans[idx][0], spans[idx + len(needle) - 1][1]))
            pos = idx + len(needle)
    return hits


def op_replace(
    data: bytes,
    old: str,
    new: str,
    expect: Optional[int] = 1,
    table_no: Optional[int] = None,
    dry_run: bool = False,
) -> Tuple[bytes, str]:
    if not old:
        raise DsdError("--old must not be empty")
    doc = Doc(data)
    root = next(doc.root.iter("BODY"), doc.root)
    scope = "document body"
    if table_no is not None:
        root, scope = doc.table(table_no).node, f"T{table_no}"
    repl = xml_escape_text(new).encode("utf-8")
    hits = [(a, b, repl) for a, b in find_text_hits(doc, old, root)]
    unit_cells = [c for t in doc.tables for c in t.cells if c.node.tag == "TU" and c.node.attrs.get("AUNITVALUE")]

    def unit_note(offset: int) -> str:
        for c in unit_cells:
            if c.node.open_end <= offset < c.node.close_start:
                return f" [TU AUNITVALUE={c.node.attrs['AUNITVALUE']} is not changed by replace]"
        return ""

    # With --table the root is the TABLE; an unverified container around it
    # encloses every hit, so it counts too.
    unverified = list(unverified_nodes(root)) + [a for a in root.ancestors() if a.tag not in VERIFIED_TAGS]

    def unverified_note(offset: int) -> str:
        tags = sorted({n.tag for n in unverified if n.open_end <= offset < n.close_start})
        return f" [inside unverified {', '.join(f'<{t}>' for t in tags)}]" if tags else ""

    flagged = [unverified_note(a) for a, _, _ in hits]
    notes = [unit_note(a) + f for (a, _, _), f in zip(hits, flagged)]
    schema = unverified_schema(doc)
    if dry_run:
        lines = [f"{len(hits)} occurrence(s) of {old!r} in {scope}"]
        if schema:
            lines.append(f"  note: schema {schema} is unverified; replace will refuse this file")
        for (a, b, _), note in list(zip(hits, notes))[:200]:
            lo, hi = max(0, a - 40), min(len(data), b + 40)
            ctx = data[lo:hi].decode("utf-8", "ignore").replace("\r", "").replace("\n", "⏎")
            lines.append(f"  line {doc.line(a)}: …{ctx}…{note}")
        return data, "\n".join(lines)
    if not hits:
        raise DsdError(
            f"{old!r} not found in text of {scope}; check spacing with `find`"
            " (attribute values and tags are never matched)"
        )
    # Refuse unverified targets before the count check, so fixing --expect is
    # never a wasted round.
    if schema:
        raise DsdError(f"document schema {schema} is not one this helper was verified on; {RAW_ROUTE}")
    inside = [doc.line(a) for (a, _, _), f in zip(hits, flagged) if f]
    if inside:
        raise DsdError(
            f"{len(inside)} occurrence(s) lie inside elements this helper has not verified (lines {inside[:15]});"
            f" {RAW_ROUTE}"
        )
    if expect is not None and len(hits) != expect:
        lines = ", ".join(str(doc.line(h[0])) for h in hits[:15])
        raise DsdError(
            f"found {len(hits)} occurrence(s) of {old!r} in {scope}, expected {expect} (lines {lines});"
            " narrow with --table, or set --expect to the verified count"
        )
    lines = sorted({doc.line(h[0]) for h in hits})
    msg = f"replaced {len(hits)} occurrence(s) of {old!r} with {new!r} at lines {lines[:20]}"
    tu_hits = sum(1 for a, _, _ in hits if unit_note(a))
    if tu_hits:
        msg += f"\n  note: {tu_hits} occurrence(s) were in TU cells; their AUNITVALUE was left as is (use set-cell --unit-value)"
    return _replace_ranges(data, hits), msg


def _row_has_rowspan_involvement(table: Table, row: int) -> bool:
    for cell in table.cells:
        if cell.rowspan > 1 and cell.row <= row < cell.row + cell.rowspan:
            return True
    return False


def _row_sep(doc: Doc, tr: Node) -> bytes:
    """Whitespace that separates this TR from the previous sibling element."""
    siblings = tr.parent.children if tr.parent else []
    i = siblings.index(tr)
    prev_end = siblings[i - 1].end if i > 0 else tr.parent.open_end  # type: ignore[union-attr]
    gap = doc.data[prev_end : tr.start]
    return gap if not gap.strip() else doc.newline()


def op_copy_row(
    data: bytes, table_no: int, row: int, count: int = 1, clear: bool = False, force: bool = False
) -> Tuple[bytes, str]:
    if count < 1:
        raise DsdError("--count must be at least 1")
    doc = Doc(data)
    table = doc.table(table_no)
    _require_verified(doc, table.node, f"T{table_no}")
    if not 1 <= row <= len(table.rows):
        raise DsdError(f"T{table_no} has rows 1..{len(table.rows)}")
    tr = table.rows[row - 1]
    if tr.attrs.get("ACOPY", "Y").upper() == "N" and not force:
        raise DsdError(f"T{table_no}R{row} is marked ACOPY=\"N\" (row copying locked by the form)")
    if _row_has_rowspan_involvement(table, row):
        raise DsdError(f"T{table_no}R{row} participates in a ROWSPAN; copy rows in DART Editor or edit XML by hand")
    row_bytes = data[tr.start : tr.end]
    note = ""
    if clear:
        sub = Doc(row_bytes)
        clears: List[Tuple[int, int, bytes]] = []
        kept: List[str] = []
        col = 0
        for cnode in sub.root.children:
            if cnode.tag not in CELL_TAGS:
                continue
            col += 1
            leaf = _leaf_for_text(cnode)
            unit_attr = cnode.tag == "TU" and cnode.attrs.get("AUNITVALUE")
            if leaf is None:
                # Mixed content: empty every text run but keep the element structure.
                clears.extend((a, b, b"") for a, b in _text_runs(cnode) if row_bytes[a:b].strip())
                kept.append(f"cell {col} (structure kept, text emptied)")
            elif leaf.close_start != leaf.open_end:
                clears.append((leaf.open_end, leaf.close_start, b""))
            if unit_attr:
                clears.append(_set_attr(row_bytes, cnode, "AUNITVALUE", ""))
        row_bytes = _replace_ranges(row_bytes, clears)
        note = " (cleared" + (f"; {', '.join(kept)}" if kept else "") + ")"
    sep = _row_sep(doc, tr)
    insertion = (sep + row_bytes) * count
    out = data[: tr.end] + insertion + data[tr.end :]
    return out, f"T{table_no}: copied R{row} {count} time(s) as R{row + 1}..R{row + count}{note}"


def op_delete_row(data: bytes, table_no: int, row: int, force: bool = False) -> Tuple[bytes, str]:
    doc = Doc(data)
    table = doc.table(table_no)
    _require_verified(doc, table.node, f"T{table_no}")
    if not 1 <= row <= len(table.rows):
        raise DsdError(f"T{table_no} has rows 1..{len(table.rows)}")
    tr = table.rows[row - 1]
    if tr.attrs.get("ADELETE", "Y").upper() == "N" and not force:
        raise DsdError(f"T{table_no}R{row} is marked ADELETE=\"N\" (row deletion locked by the form)")
    if _row_has_rowspan_involvement(table, row):
        raise DsdError(f"T{table_no}R{row} participates in a ROWSPAN; delete it in DART Editor or edit XML by hand")
    if tr.parent is not None and len([c for c in tr.parent.children if c.tag == "TR"]) == 1:
        raise DsdError(f"T{table_no}R{row} is the only row in its {tr.parent.tag}; refusing to leave it empty")
    sep = _row_sep(doc, tr)
    start = tr.start - len(sep) if data[tr.start - len(sep) : tr.start] == sep else tr.start
    text = " | ".join(node_text(c.node) for c in table.cells if c.row == row)
    return data[:start] + data[tr.end :], f"T{table_no}: deleted R{row} ({text[:80]!r})"


OP_FIELDS: Dict[str, Dict[str, Tuple[type, bool]]] = {
    # field: (type, required)
    "set-cell": {"table": (int, True), "row": (int, True), "col": (int, True), "text": (str, True),
                 "unit_value": (str, False), "flatten": (bool, False)},
    "replace": {"old": (str, True), "new": (str, True), "expect": (object, False), "table": (int, False)},
    "copy-row": {"table": (int, True), "row": (int, True), "count": (int, False), "clear": (bool, False),
                 "force_locked": (bool, False)},
    "delete-row": {"table": (int, True), "row": (int, True), "force_locked": (bool, False)},
}


def _validated(op: Dict[str, object]) -> Dict[str, object]:
    kind = op.get("op")
    if kind not in OP_FIELDS:
        raise DsdError(f"unknown op {kind!r}; use {', '.join(OP_FIELDS)}")
    spec = OP_FIELDS[str(kind)]
    unknown = set(op) - set(spec) - {"op"}
    if unknown:
        raise DsdError(f"unknown field(s) {sorted(unknown)}; {kind} accepts {sorted(spec)}")
    for name, (typ, required) in spec.items():
        if name not in op or op[name] is None:
            if required:
                raise DsdError(f"missing field {name!r}")
            continue
        value = op[name]
        if typ is int and (not isinstance(value, int) or isinstance(value, bool)):
            raise DsdError(f"field {name!r} must be an integer, got {value!r}")
        if typ is bool and not isinstance(value, bool):
            raise DsdError(f"field {name!r} must be true or false, got {value!r}")
        if typ is str and not isinstance(value, str):
            raise DsdError(f"field {name!r} must be a string, got {value!r}")
    if kind == "replace":
        op = dict(op)
        op["expect"] = _parse_expect(op.get("expect", 1))
    return op


def _parse_expect(value: object) -> Optional[int]:
    if value in (None, "any"):
        return None
    if isinstance(value, bool):
        raise DsdError(f"expect must be a positive integer or 'any', got {value!r}")
    try:
        number = int(value)  # type: ignore[call-overload]
    except (TypeError, ValueError):
        raise DsdError(f"expect must be a positive integer or 'any', got {value!r}") from None
    if number < 1:
        raise DsdError(f"expect must be a positive integer or 'any', got {value!r}")
    return number


def apply_ops(data: bytes, ops: Sequence[Dict[str, object]]) -> Tuple[bytes, List[str]]:
    log: List[str] = []
    for i, raw in enumerate(ops, 1):
        kind = raw.get("op")
        try:
            op: Dict[str, object] = _validated(raw)
            g = op.get
            if kind == "set-cell":
                data, msg = op_set_cell(data, g("table"), g("row"), g("col"), g("text"),  # type: ignore[arg-type]
                                        unit_value=g("unit_value"), flatten=bool(g("flatten")))  # type: ignore[arg-type]
            elif kind == "replace":
                data, msg = op_replace(data, g("old"), g("new"), expect=g("expect"),  # type: ignore[arg-type]
                                       table_no=g("table"))  # type: ignore[arg-type]
            elif kind == "copy-row":
                data, msg = op_copy_row(data, g("table"), g("row"), count=g("count") or 1,  # type: ignore[arg-type]
                                        clear=bool(g("clear")), force=bool(g("force_locked")))
            else:
                data, msg = op_delete_row(data, g("table"), g("row"), force=bool(g("force_locked")))  # type: ignore[arg-type]
        except DsdError as exc:
            raise DsdError(f"edit #{i} ({kind}): {exc}") from None
        log.append(msg)
    return data, log


# --------------------------------------------------------------------------
# Text view (`read`)
#
# One line per block: contents.xml line, element path, then either the exact
# document text as a JSON string or, for tables, a stub pointing at `table`.
# Rules exist only for verified elements in known positions; everything else
# takes the generic rule, which shows more rather than less: the element gets
# its own line with its attributes, and its children follow on further lines.

LINE_BREAK_MARK = "⏎"  # shown for DART's &cr; line break
INLINE_TAGS = frozenset({"SPAN", "A"})  # their text joins the enclosing block's text
TEXT_VIEW_TAGS = frozenset({
    "P", "TITLE", "COVER-TITLE", "IMG", "IMG-CAPTION", "WARNING",
    "DOCUMENT-NAME", "FORMULA-VERSION", "COMPANY-NAME", "EXTRACTION",
})
# A region starts a fresh path, so paths stay short: SECTION-2/P rather than
# BODY/INSERTION/LIBRARY/SECTION-1/SECTION-2/P. `outline` shows the nesting.
REGION_TAGS = frozenset({"DOCUMENT-HEADER", "COVER", "TOC", "PART", "APPENDIX"} | {f"SECTION-{n}" for n in range(1, 8)})
TRANSPARENT_TAGS = frozenset({"DOCUMENT", "BODY"})
CONTAINER_TAGS = REGION_TAGS | TRANSPARENT_TAGS | {"SUMMARY", "INSERTION", "LIBRARY", "TABLE-GROUP", "IMAGE"}
SHOWN_ATTRS = {"DOCUMENT-NAME": ("ACODE",), "EXTRACTION": ("ACODE",)}  # attributes that carry meaning


def _layout_free(text: str) -> str:
    """Drop literal newlines, which are XML layout; DART stores breaks as &cr;."""
    return text.replace("\r", "").replace("\n", "")


def _attr_label(node: Node, names: Optional[Sequence[str]] = None) -> str:
    items = [(k, v) for k, v in node.attrs.items() if names is None or k in names]
    if not items:
        return ""
    plain = re.compile(r"[^\s\[\]\"=]+")
    return "[" + " ".join(f"{k}={v if plain.fullmatch(v) else json.dumps(v, ensure_ascii=False)}" for k, v in items) + "]"


LAYOUT_SPACE = " \t\r\n"  # not U+3000 or NBSP, which DART uses as visible content
LEADING_LAYOUT_RE = re.compile(rb"[ \t\r\n]*")


def exact_text(node: Node, scaffold: bool = False, tables: Optional[Dict[Node, Table]] = None) -> str:
    """A block's text without whitespace normalization.

    &cr; becomes LINE_BREAK_MARK. SPAN/A text joins in place; any other child
    element is kept visible as ⟦TAG[attrs]⟧…⟦/TAG⟧ rather than silently merged,
    and a table as a ⟦T#⟧ stub so its cells keep their `table` coordinates.
    """
    parts: List[str] = []
    for item in node.items:
        if isinstance(item, str):
            parts.append(_layout_free(item).replace(CR_TOKEN, LINE_BREAK_MARK))
        elif item.tag in SCAFFOLD_TAGS and not scaffold:
            continue
        elif item.tag in INLINE_TAGS:
            parts.append(exact_text(item, scaffold, tables))
        elif tables is not None and item in tables:
            parts.append(f"⟦T{tables[item].index} -> table {tables[item].index}⟧")
        else:
            parts.append(f"⟦{item.tag}{_attr_label(item)}⟧{exact_text(item, scaffold, tables)}⟦/{item.tag}⟧")
    return "".join(parts)


class TextView:
    def __init__(self, doc: Doc, scaffold: bool = False):
        self.doc = doc
        self.scaffold = scaffold
        self.table_by_node = {t.node: t for t in doc.tables}
        self.rows: List[Tuple[int, str, str]] = []  # (line, path, rest)

    def render(self) -> str:
        self.visit(self.doc.root, [], keep_path=False)
        width = min(max((len(p) for _, p, _ in self.rows), default=0), 40)
        lines = [f"L{line:<6} {path:<{width}} {rest}".rstrip() for line, path, rest in self.rows]
        return "\n".join(lines) + ("\n" if lines else "")

    def add(self, offset: int, path: List[str], rest: str) -> None:
        self.rows.append((self.doc.line(offset), "/".join(path), rest))

    def walk(self, node: Node, path: List[str], keep_path: bool) -> None:
        pos = node.open_end
        for item in node.items:
            if isinstance(item, str):
                text = _layout_free(item)
                if text.strip(LAYOUT_SPACE):
                    # Expat folds CRLF into LF, so measure the layout in the raw bytes.
                    line_pos = LEADING_LAYOUT_RE.match(self.doc.data, pos).end()  # type: ignore[union-attr]
                    self.add(line_pos, path + ["#text"], _q(text.replace(CR_TOKEN, LINE_BREAK_MARK)))
                continue
            self.visit(item, path, keep_path)
            pos = item.end

    def visit(self, node: Node, path: List[str], keep_path: bool) -> None:
        tag = node.tag
        if tag in SCAFFOLD_TAGS:
            if self.scaffold:
                self.add(node.start, path + [tag], _q(exact_text(node, True, self.table_by_node)))
            return
        known = tag in VERIFIED_TAGS
        if known and tag == "TABLE":
            table = self.table_by_node[node]
            stub = f"T{table.index} rows={len(table.rows)} cols={table.ncols} -> table {table.index}"
            unseen = unverified_tags(node)
            if unseen:
                stub += f" [contains unverified {','.join(unseen)}]"
            self.add(node.start, path + [tag], stub)
        elif known and tag == "PGBRK" and not exact_text(node, self.scaffold).strip(LAYOUT_SPACE) and not node.children:
            self.add(node.start, path + [tag], "")
        elif known and tag in TEXT_VIEW_TAGS:
            self.add(node.start, path + [tag + _attr_label(node, SHOWN_ATTRS.get(tag, ()))],
                     _q(exact_text(node, self.scaffold, self.table_by_node)))
        elif known and tag in CONTAINER_TAGS:
            if tag in TRANSPARENT_TAGS:
                inner = path
            elif tag in REGION_TAGS and not keep_path:
                inner = [tag]
            else:
                inner = path + [tag]
            self.walk(node, inner, keep_path)
        else:
            # Generic rule: unverified elements, and verified ones somewhere the
            # rules above do not expect them (a TR outside any TABLE, say).
            seg = path + [tag + _attr_label(node)]
            shown = [c for c in node.children if self.scaffold or c.tag not in SCAFFOLD_TAGS]
            if all(c.tag in INLINE_TAGS for c in shown):
                self.add(node.start, seg, _q(exact_text(node, self.scaffold, self.table_by_node)))
            else:
                self.add(node.start, seg, "")
                self.walk(node, seg, keep_path=True)


def read_header(arc: Archive, doc: Doc) -> str:
    head = header_info(doc)
    editor = ""
    try:
        gen = parse_meta(arc.meta).find("GENERATOR")
        editor = gen.get("editver", "") if gen is not None else ""
    except DsdError:
        pass
    fields: List[Tuple[str, object]] = [
        ("source", arc.path.name),
        ("schema", head.get("schema", "")),
        ("editor_version", editor),
        ("tables", len(doc.tables)),
        ("images", len(arc.images())),
        ("converter", f"dsd.py {VERSION}"),
    ]
    unverified: List[str] = unverified_tags(doc.root)
    schema = unverified_schema(doc)
    if schema:
        unverified.insert(0, f"schema {schema}")
    if unverified:
        fields.append(("unverified", unverified))
    fields.append(("legend", f"L<line> <path>[attrs] \"exact text\"; {LINE_BREAK_MARK} = &cr; line break;"
                             " ⟦TAG⟧…⟦/TAG⟧ = element inside text; T<n> -> run `table FILE n` for cells"))
    return "".join(f"# {k}: {json.dumps(v, ensure_ascii=False)}\n" for k, v in fields) + "\n"


# --------------------------------------------------------------------------
# Commands


def _print(text: str = "") -> None:
    sys.stdout.write(text + "\n")


def _issues_report(issues: List[Issue]) -> str:
    if not issues:
        return "check: ok (no issues)"
    counts = {lvl: sum(1 for i in issues if i.level == lvl) for lvl in ("error", "warning", "info")}
    lines = [f"check: {counts['error']} error(s), {counts['warning']} warning(s), {counts['info']} info"]
    lines += [f"  {i.level:7} {i.code:18} {i.message}" for i in issues]
    return "\n".join(lines)


def cmd_inspect(args: argparse.Namespace) -> int:
    arc = read_archive(args.file)
    issues, doc = check_archive(arc)
    report: Dict[str, object] = {
        "file": arc.path.name,
        "bytes": arc.path.stat().st_size,
        "members": [
            {
                "name": m.name,
                "name_encoding": m.name_encoding,
                "utf8_flag": bool(m.info.flag_bits & 0x800),
                "method": {0: "stored", 8: "deflate"}.get(m.info.compress_type, str(m.info.compress_type)),
                "size": m.info.file_size,
            }
            for m in arc.members
        ],
    }
    try:
        meta = parse_meta(arc.meta)
        gen, info = meta.find("GENERATOR"), meta.find("DOCUMENT-INFO")
        report["meta"] = {
            "schema": gen.get("schema") if gen is not None else None,
            "editor_version": gen.get("editver") if gen is not None else None,
            "doc_version": info.get("docver") if info is not None else None,
            "doc_id": info.get("doc-id") if info is not None else None,
            "is_correction": info.get("iscorrection") if info is not None else None,
        }
    except DsdError:
        pass
    if doc is not None:
        head = header_info(doc)
        counts: Dict[str, int] = {}
        for n in doc.root.iter():
            counts[n.tag] = counts.get(n.tag, 0) + 1
        keys = ("PART", "SECTION-1", "SECTION-2", "SECTION-3", "TITLE", "P", "TABLE", "TR", "TH", "TD", "TE", "TU",
                "INSERTION", "LIBRARY", "IMAGE", "PGBRK", "WARNING", "APPENDIX", "NOTE")
        report["document"] = {
            "schema": head.get("schema"),
            "type": head.get("document_name"),
            "type_code": head.get("document_code"),
            "formula_version": head.get("formula_version"),
            "line_endings": "CRLF" if b"\r\n" in doc.data else "LF",
            "counts": {k: counts[k] for k in keys if k in counts},
            "span_tables": sum(1 for t in doc.tables if any(c.rowspan > 1 or c.colspan > 1 for c in t.cells)),
        }
    report["issues"] = [i.as_dict() for i in issues]
    if args.json:
        _print(json.dumps(report, ensure_ascii=False, indent=2))
        return 0
    _print(f"{report['file']}  {report['bytes']} bytes")
    for m in report["members"]:  # type: ignore[union-attr]
        _print(f"  member {m['name']}  {m['size']} bytes  {m['method']}  name={m['name_encoding']}{' (utf8 flag)' if m['utf8_flag'] else ''}")
    if "meta" in report:
        _print("  meta   " + "  ".join(f"{k}={v}" for k, v in report["meta"].items()))  # type: ignore[union-attr]
    if "document" in report:
        d = report["document"]
        _print("  doc    " + "  ".join(f"{k}={v}" for k, v in d.items() if k != "counts"))  # type: ignore[union-attr]
        _print("  counts " + "  ".join(f"{k}={v}" for k, v in d["counts"].items()))  # type: ignore[index]
    _print(_issues_report(issues))
    return 0


def cmd_outline(args: argparse.Namespace) -> int:
    doc = Doc(read_archive(args.file).contents)
    table_by_node = {t.node: t for t in doc.tables}

    def walk(node: Node, depth: int) -> None:
        for child in node.children:
            tag = child.tag
            line = doc.line(child.start)
            pad = "  " * depth
            if tag in ("PART",) or tag.startswith("SECTION-"):
                title = next((c for c in child.children if c.tag == "TITLE"), None)
                name = " ".join(node_text(title).split()) if title is not None else ""
                flag = " [unverified element]" if tag not in VERIFIED_TAGS else ""
                _print(f"{line:>7}  {pad}{tag} {name}{flag}")
                walk(child, depth + 1)
            elif tag not in VERIFIED_TAGS:
                _print(f"{line:>7}  {pad}<{tag}> unverified element (contents shown below as found)")
                walk(child, depth + 1)
            elif tag == "COVER-TITLE":
                _print(f"{line:>7}  {pad}COVER-TITLE {' '.join(node_text(child).split())}")
            elif tag in ("COVER", "TOC", "BODY", "LIBRARY", "TABLE-GROUP", "APPENDIX"):
                label = child.attrs.get("ACLASS") or child.attrs.get("ATYPE") or ""
                if tag in ("COVER", "TOC", "APPENDIX"):
                    _print(f"{line:>7}  {pad}{tag} {label}".rstrip())
                walk(child, depth + (1 if tag in ("COVER", "TOC") else 0))
            elif tag == "INSERTION":
                comment = next((c for c in child.children if c.tag == "COMMENT"), None)
                filled = any(c.tag == "LIBRARY" for c in child.children)
                if filled or args.all:
                    note = " ".join(node_text(comment, True).split()) if comment is not None else ""
                    _print(f"{line:>7}  {pad}INSERTION {'filled' if filled else 'empty'} {note[:60]}")
                walk(child, depth)
            elif tag == "TABLE":
                t = table_by_node[child]
                first = ""
                for c in t.cells:
                    txt = " ".join(node_text(c.node).split())
                    if txt:
                        first = txt
                        break
                _print(f"{line:>7}  {pad}T{t.index} {len(t.rows)}x{t.ncols} {t.node.attrs.get('ACLASS', '')} {first[:50]}")
            elif tag == "IMAGE":
                img = next(child.iter("IMG"), None)
                _print(f"{line:>7}  {pad}IMAGE {node_text(img) if img is not None else ''}")
            elif args.all and tag in ("P", "WARNING", "TITLE") and not (
                tag == "TITLE" and (node.tag == "PART" or node.tag.startswith("SECTION-"))
            ):
                txt = " ".join(node_text(child).split())
                if txt:
                    _print(f"{line:>7}  {pad}{tag} {txt[:70]}")
            elif tag not in SCAFFOLD_TAGS and tag not in ("P", "PGBRK", "TITLE", "WARNING"):
                walk(child, depth)

    body = next(doc.root.iter("BODY"), None)
    walk(body if body is not None else doc.root, 0)
    return 0


def cmd_table(args: argparse.Namespace) -> int:
    doc = Doc(read_archive(args.file).contents)
    for number in args.tables:
        table = doc.table(number)
        n = table.node
        _print(
            f"T{table.index} rows={len(table.rows)} cols={table.ncols} line={doc.line(n.start)}"
            f" ACLASS={n.attrs.get('ACLASS', '')} BORDER={n.attrs.get('BORDER', '')} AFIXTABLE={n.attrs.get('AFIXTABLE', '')}"
            f" COL={table.cols}"
        )
        for relation, unseen in (("contains", unverified_tags(n)), ("lies inside", unverified_ancestor_tags(n))):
            if unseen:
                _print(f"  warning: {relation} unverified {', '.join(f'<{t}>' for t in unseen)}; coordinates may"
                       " not match DART Editor and edit commands refuse this table")
        for ri, tr in enumerate(table.rows, 1):
            flags = f"{tr.parent.tag if tr.parent else ''} ACOPY={tr.attrs.get('ACOPY', '')} ADELETE={tr.attrs.get('ADELETE', '')}"
            _print(f"  R{ri} line={doc.line(tr.start)} {flags}")
            for ci in range(table.ncols):
                cell = table.grid[ri - 1][ci]
                if cell is None:
                    _print(f"    C{ci + 1} <missing>")
                    continue
                if (cell.row, cell.col) != (ri, ci + 1):
                    if cell.col == ci + 1 or args.spans:
                        _print(f"    C{ci + 1} ^ covered by R{cell.row}C{cell.col}")
                    continue
                extra = []
                if cell.rowspan > 1:
                    extra.append(f"rowspan={cell.rowspan}")
                if cell.colspan > 1:
                    extra.append(f"colspan={cell.colspan}")
                for key in ("ACODE", "AUNIT", "AUNITVALUE"):
                    if key in cell.node.attrs:
                        extra.append(f"{key}={cell.node.attrs[key]}")
                leaf = _leaf_for_text(cell.node)
                if leaf is None:
                    extra.append("mixed-content")
                text = node_text(cell.node).replace("\n", "⏎")
                _print(f"    C{ci + 1} {cell.node.tag} {json.dumps(text, ensure_ascii=False)} {' '.join(extra)}".rstrip())
    return 0


def cmd_find(args: argparse.Namespace) -> int:
    doc = Doc(read_archive(args.file).contents)
    target = args.text if args.exact else compact(args.text)
    hits = 0
    for block in iter_blocks(doc):
        hay = block.text if args.exact else compact(block.text)
        if target and target in hay:
            hits += 1
            snippet = " ".join(block.text.split())
            flag = f" [unverified {','.join(block.unverified)}]" if block.unverified else ""
            _print(f"{block.locator:>14}  line={block.line:<6} {block.tag:11} {snippet[:120]}{flag}")
            if hits >= args.limit:
                _print(f"... stopped after {args.limit} matches (raise --limit)")
                break
    if not hits:
        _print("no matches")
        return 1
    return 0


def cmd_read(args: argparse.Namespace) -> int:
    arc = read_archive(args.file)
    doc = Doc(arc.contents)
    out_path = Path(args.output) if args.output and args.output != "-" else None
    if out_path is not None:
        _guard_output(out_path, args.force, inputs=[arc.path])
        if _same_file(out_path, arc.path):
            raise DsdError("refusing to write the text view over the source .dsd")
    view = TextView(doc, scaffold=args.scaffold)
    text = (read_header(arc, doc) if not args.no_header else "") + view.render()
    if out_path is None:
        sys.stdout.write(text)
    else:
        _atomic_write(out_path, text.encode("utf-8"))
        _print(f"wrote {out_path} ({len(view.rows)} lines, {len(doc.tables)} tables)")
    return 0


def cmd_check(args: argparse.Namespace) -> int:
    arc = read_archive(args.file)
    issues, doc = check_archive(arc)
    if doc is not None and not args.no_schema:
        schema_dir = Path(args.schema_dir) if args.schema_dir else default_schema_dir()
        schema = header_info(doc).get("schema", "")
        if args.schema_dir and not schema_dir.is_dir():  # type: ignore[union-attr]
            raise DsdError(f"{args.schema_dir}: schema directory not found")
        if not schema:
            issues.append(Issue("info", "schema-skipped", "contents.xml declares no schema; schema validation skipped"))
        elif schema_dir is None:
            issues.append(Issue(
                "info", "schema-skipped",
                "no DART schema directory found (pass --schema-dir or set DART_SCHEMA_DIR); schema validation skipped",
            ))
        else:
            issues.extend(schema_validate(arc.contents, schema_dir, schema))
    if args.against:
        issues.extend(compare_archives(read_archive(args.against), arc))
    if args.json:
        _print(json.dumps([i.as_dict() for i in issues], ensure_ascii=False, indent=2))
    else:
        _print(_issues_report(issues))
    return 1 if any(i.level == "error" for i in issues) else 0


def compare_members(orig: Archive, new: Archive) -> List[Issue]:
    out: List[Issue] = []
    on = [(m.raw_name, m.info.flag_bits & 0x800) for m in orig.members]
    nn = [(m.raw_name, m.info.flag_bits & 0x800) for m in new.members]
    if on != nn:
        out.append(Issue("warning", "members-changed", "member names, order, or name flags differ from the original"))
    for om in orig.members:
        nm = next((m for m in new.members if m.raw_name == om.raw_name), None)
        if nm is not None and om.name != CONTENTS and nm.data != om.data:
            out.append(Issue("warning", "member-data", f"{om.name} differs from the original"))
    return out


def line_ending_issue(old: bytes, new: bytes) -> Optional[Issue]:
    crlf_old, lf_old = old.count(b"\r\n"), old.count(b"\n")
    crlf_new, lf_new = new.count(b"\r\n"), new.count(b"\n")
    if crlf_old and crlf_old == lf_old and crlf_new != lf_new:
        bare = lf_new - crlf_new
        return Issue("error", "line-endings", f"contents.xml gained {bare} bare LF line ending(s); the original is CRLF throughout")
    return None


def compare_archives(orig: Archive, new: Archive) -> List[Issue]:
    out = compare_members(orig, new)
    eol = line_ending_issue(orig.contents, new.contents)
    if eol:
        out.append(eol)
    odoc, ndoc = Doc(orig.contents), Doc(new.contents)
    for tag in ("TABLE", "SECTION-1", "SECTION-2", "TITLE", "IMAGE", "INSERTION", "LIBRARY"):
        a, b = sum(1 for _ in odoc.root.iter(tag)), sum(1 for _ in ndoc.root.iter(tag))
        if a != b:
            out.append(Issue("info", "structure", f"{tag} count {a} -> {b}"))
    a, b = sum(len(t.rows) for t in odoc.tables), sum(len(t.rows) for t in ndoc.tables)
    if a != b:
        out.append(Issue("info", "structure", f"table rows {a} -> {b}"))
    changed = sum(1 for _ in block_diff(odoc, ndoc))
    raw = sum(max(i2 - i1, j2 - j1) for i1, i2, j1, j2 in raw_line_changes(orig.contents, new.contents))
    out.append(Issue("info", "changes", f"{changed} changed cell/text block(s), {raw} changed raw line(s); review with `dsd.py diff`"))
    return out


DiffEntry = Tuple[str, Optional[Block], Optional[Block]]


def _units(doc: Doc) -> List[List[Block]]:
    """Blocks grouped into diff units: one per table row, one per text block."""
    units: List[List[Block]] = []
    for block in iter_blocks(doc):
        if units and units[-1][0].group == block.group:
            units[-1].append(block)
        else:
            units.append([block])
    return units


def _pair_blocks(olds: Sequence[Block], news: Sequence[Block]) -> Iterator[DiffEntry]:
    if len(olds) == len(news):  # same shape: an in-place edit, so pair by position
        for o, n in zip(olds, news):
            if o.key() != n.key():
                yield "changed", o, n
        return
    sm = difflib.SequenceMatcher(None, [b.key() for b in olds], [b.key() for b in news], autojunk=False)
    for op, i1, i2, j1, j2 in sm.get_opcodes():
        if op == "equal":
            continue
        a, b = olds[i1:i2], news[j1:j2]
        for k in range(max(len(a), len(b))):
            o = a[k] if k < len(a) else None
            n = b[k] if k < len(b) else None
            yield ("changed" if o and n else "removed" if o else "added"), o, n


def _unit_kind(unit: Sequence[Block]) -> str:
    """Units of one kind may pair: rows of the same table, or text blocks with the same tag."""
    group = unit[0].group
    return group.split("R", 1)[0] if group.startswith("T") else f"<{unit[0].tag}>"


def _unit_similarity(o: Sequence[Block], n: Sequence[Block]) -> float:
    """Cells equal at the same position, plus a 0..1 text resemblance tie-breaker."""
    same = sum(a.key() == b.key() for a, b in zip(o, n))
    texts = difflib.SequenceMatcher(None, "\x1f".join(b.text for b in o), "\x1f".join(b.text for b in n), autojunk=False)
    return same + texts.ratio()


# Above this many candidate unit pairs in one changed region, pairing falls
# back to position; such regions are wholesale rewrites, not row edits.
MAX_PAIRING_WORK = 40000


def _align_units(olds: Sequence[List[Block]], news: Sequence[List[Block]]) -> Iterator[DiffEntry]:
    """Diff a changed region whose row counts differ.

    Each old unit pairs with at most one new unit, in order, maximizing total
    similarity; only paired units are compared cell by cell, and unpaired ones
    are reported whole. A cleared copy-row keeps its template's attributes, so
    attributes alone cannot tell an inserted row from an edited neighbour.
    """
    m, n = len(olds), len(news)
    if m * n > MAX_PAIRING_WORK:
        for k in range(max(m, n)):
            yield from _pair_blocks(olds[k] if k < m else [], news[k] if k < n else [])
        return
    score = [[0.0] * (n + 1) for _ in range(m + 1)]
    for i in range(m - 1, -1, -1):
        for j in range(n - 1, -1, -1):
            best = max(score[i + 1][j], score[i][j + 1])
            if _unit_kind(olds[i]) == _unit_kind(news[j]):
                best = max(best, score[i + 1][j + 1] + _unit_similarity(olds[i], news[j]))
            score[i][j] = best
    i = j = 0
    while i < m or j < n:
        if i < m and j < n and _unit_kind(olds[i]) == _unit_kind(news[j]) and (
            score[i][j] == score[i + 1][j + 1] + _unit_similarity(olds[i], news[j])
        ):
            yield from _pair_blocks(olds[i], news[j])
            i, j = i + 1, j + 1
        elif i < m and (j == n or score[i][j] == score[i + 1][j]):
            yield from _pair_blocks(olds[i], [])
            i += 1
        else:
            yield from _pair_blocks([], news[j])
            j += 1


def block_diff(odoc: Doc, ndoc: Doc) -> Iterator[DiffEntry]:
    """Changed blocks, aligned row by row before cell by cell.

    Matching individual cells lets an empty cell at the end of one row pair
    with an identical empty cell at the end of an inserted row, which reports
    the insertion one cell early. Aligning whole rows first keeps an inserted
    or deleted row in one piece at its real coordinates.
    """
    ou, nu = _units(odoc), _units(ndoc)

    def key(unit: List[Block]) -> Tuple[Tuple[str, str, Tuple[Tuple[str, str], ...]], ...]:
        return tuple(b.key() for b in unit)

    sm = difflib.SequenceMatcher(None, [key(u) for u in ou], [key(u) for u in nu], autojunk=False)
    for op, i1, i2, j1, j2 in sm.get_opcodes():
        if op == "equal":
            continue
        if op == "replace" and i2 - i1 == j2 - j1:
            for o_unit, n_unit in zip(ou[i1:i2], nu[j1:j2]):
                yield from _pair_blocks(o_unit, n_unit)
        else:
            yield from _align_units(ou[i1:i2], nu[j1:j2])


def raw_line_changes(old: bytes, new: bytes) -> List[Tuple[int, int, int, int]]:
    """Changed line ranges (1-based, half-open) between two contents.xml versions."""
    a, b = old.splitlines(), new.splitlines()
    sm = difflib.SequenceMatcher(None, a, b, autojunk=False)
    return [(i1 + 1, i2 + 1, j1 + 1, j2 + 1) for op, i1, i2, j1, j2 in sm.get_opcodes() if op != "equal"]


def _q(text: str) -> str:
    return json.dumps(text, ensure_ascii=False)


def cmd_diff(args: argparse.Namespace) -> int:
    orig, edited = read_archive(args.original), read_archive(args.edited)
    odoc, ndoc = Doc(orig.contents), Doc(edited.contents)
    count = 0
    for kind, o, n in block_diff(odoc, ndoc):
        count += 1
        if count > args.limit:
            continue
        if kind == "changed":
            assert o is not None and n is not None
            parts = []
            if o.text != n.text:
                parts.append(f"{_q(o.text)} -> {_q(n.text)}")
            if o.attrs != n.attrs:
                od, nd = dict(o.attrs), dict(n.attrs)
                for key in sorted(set(od) | set(nd)):
                    if od.get(key) != nd.get(key):
                        parts.append(f"@{key} {_q(od.get(key, '(none)'))} -> {_q(nd.get(key, '(none)'))}")
            if o.tag != n.tag:
                parts.append(f"<{o.tag}> -> <{n.tag}>")
            _print(f"~ {o.locator} -> {n.locator}  {'  '.join(parts)}")
        elif kind == "removed":
            assert o is not None
            _print(f"- {o.locator} {o.tag} {_q(o.text)}")
        else:
            assert n is not None
            _print(f"+ {n.locator} {n.tag} {_q(n.text)}")
    if count > args.limit:
        _print(f"... {count - args.limit} more change(s) (raise --limit)")
    _print(f"{count} changed block(s) in cells and text blocks")
    ranges = raw_line_changes(orig.contents, edited.contents)
    changed_lines = sum(max(i2 - i1, j2 - j1) for i1, i2, j1, j2 in ranges)
    spots = ", ".join(f"{j1}" + (f"-{j2 - 1}" if j2 - j1 > 1 else "") for _, _, j1, j2 in ranges[:20])
    _print(f"contents.xml: {changed_lines} raw line(s) changed" + (f" at edited lines {spots}" if ranges else ""))
    if args.raw:
        olines, nlines = orig.contents.splitlines(), edited.contents.splitlines()
        for i1, i2, j1, j2 in ranges[: args.limit]:
            for k in range(i1, i2):
                _print(f"  -{k:>6} {olines[k - 1].decode('utf-8', 'replace')}")
            for k in range(j1, j2):
                _print(f"  +{k:>6} {nlines[k - 1].decode('utf-8', 'replace')}")
    for issue in compare_members(orig, edited):
        _print(f"{issue.code}: {issue.message}")
    return 0


def _issue_key(issue: Issue) -> Tuple[str, str, str]:
    # Line numbers shift with any edit above an issue; compare without them.
    return issue.level, issue.code, re.sub(r"line \d+", "line ?", issue.message)


def _finish_write(src: Archive, replacements: Dict[str, bytes], out: Path, force: bool, log: Iterable[str]) -> int:
    """Write `src` with member replacements unless that adds structural errors."""
    _guard_output(out, force, inputs=[src.path])
    before, _ = check_archive(src)
    probe = Archive(src.path, [
        Member(m.info, m.raw_name, m.name, m.name_encoding, replacements.get(m.name, m.data)) for m in src.members
    ])
    after, _ = check_archive(probe)
    eol = line_ending_issue(src.contents, probe.contents)
    if eol:
        after.append(eol)
    seen: Dict[Tuple[str, str, str], int] = {}
    for issue in before:
        seen[_issue_key(issue)] = seen.get(_issue_key(issue), 0) + 1
    fresh: List[Issue] = []
    for issue in after:
        key = _issue_key(issue)
        if seen.get(key, 0) > 0:
            seen[key] -= 1
        else:
            fresh.append(issue)
    for line in log:
        _print(line)
    new_errors = [i for i in fresh if i.level == "error"]
    if new_errors:
        _print(_issues_report(new_errors))
        raise DsdError("edit would introduce structural errors; nothing was written")
    for i in fresh:
        if i.level == "warning":
            _print(f"  warning {i.code:18} {i.message}")
    write_archive(src, replacements, out, force=force)
    reread = read_archive(out)
    for name, data in replacements.items():
        if reread.get(name).data != data:
            raise DsdError(f"{out}: verification failed after writing {name}")
    _print(f"wrote {out}")
    return 0


def _finish_edit(src: Archive, new_contents: bytes, out: Path, force: bool, log: Iterable[str]) -> int:
    return _finish_write(src, {CONTENTS: new_contents}, out, force, log)


def cmd_set_cell(args: argparse.Namespace) -> int:
    src = read_archive(args.file)
    data, msg = op_set_cell(src.contents, args.table, args.row, args.col, args.text, args.unit_value, args.flatten)
    return _finish_edit(src, data, Path(args.output), args.force, [msg])


def cmd_replace(args: argparse.Namespace) -> int:
    src = read_archive(args.file)
    expect = _parse_expect(args.expect)
    data, msg = op_replace(src.contents, args.old, args.new, expect=expect, table_no=args.table, dry_run=args.dry_run)
    if args.dry_run:
        _print(msg)
        return 0
    if not args.output:
        raise DsdError("-o/--output is required unless --dry-run is given")
    return _finish_edit(src, data, Path(args.output), args.force, [msg])


def cmd_copy_row(args: argparse.Namespace) -> int:
    src = read_archive(args.file)
    data, msg = op_copy_row(src.contents, args.table, args.row, args.count, args.clear, args.force_locked)
    return _finish_edit(src, data, Path(args.output), args.force, [msg])


def cmd_delete_row(args: argparse.Namespace) -> int:
    src = read_archive(args.file)
    data, msg = op_delete_row(src.contents, args.table, args.row, args.force_locked)
    return _finish_edit(src, data, Path(args.output), args.force, [msg])


def cmd_apply(args: argparse.Namespace) -> int:
    src = read_archive(args.file)
    try:
        ops = json.loads(Path(args.edits).read_text(encoding="utf-8-sig"))
    except (OSError, json.JSONDecodeError) as exc:
        raise DsdError(f"{args.edits}: cannot read edit list ({exc})") from None
    if not isinstance(ops, list) or not all(isinstance(o, dict) for o in ops):
        raise DsdError(f"{args.edits}: expected a JSON array of edit objects")
    data, log = apply_ops(src.contents, ops)
    return _finish_edit(src, data, Path(args.output), args.force, log)


def cmd_extract(args: argparse.Namespace) -> int:
    arc = read_archive(args.file)
    out = Path(args.directory)
    if out.exists() and not out.is_dir():
        raise DsdError(f"{out}: exists and is not a directory")
    if out.exists() and any(out.iterdir()) and not args.force:
        raise DsdError(f"{out}: directory is not empty; pass --force only if replacing it is authorized")
    out.mkdir(parents=True, exist_ok=True)
    manifest = {"source": arc.path.name, "tool": f"dsd.py {VERSION}", "members": []}
    for i, m in enumerate(arc.members):
        if m.name in (CONTENTS, META):
            rel = m.name
        else:
            safe = re.sub(r'[\\/:*?"<>|\x00-\x1f]', "_", m.name)
            rel = f"members/{i:03d}-{safe}"
        _atomic_write(out / rel, m.data)
        manifest["members"].append({"name": m.name, "file": rel, "raw_name_hex": m.raw_name.hex()})  # type: ignore[union-attr]
    _atomic_write(out / "dsd-manifest.json", json.dumps(manifest, ensure_ascii=False, indent=2).encode("utf-8"))
    _print(f"extracted {len(arc.members)} member(s) to {out}; edit {out / CONTENTS} and repack with `dsd.py pack`")
    return 0


def cmd_pack(args: argparse.Namespace) -> int:
    src = read_archive(args.original)
    directory = Path(args.directory)
    manifest_path = directory / "dsd-manifest.json"
    if not manifest_path.is_file():
        raise DsdError(f"{manifest_path}: missing; extract with `dsd.py extract` first")
    manifest = json.loads(manifest_path.read_text(encoding="utf-8"))
    by_raw = {m.raw_name.hex(): m for m in src.members}
    listed = [e["raw_name_hex"] for e in manifest["members"]]
    if sorted(listed) != sorted(by_raw):
        raise DsdError("the manifest does not match the original archive's members; re-extract from the original")
    replacements: Dict[str, bytes] = {}
    for entry in manifest["members"]:
        path = directory / entry["file"]
        if not path.is_file():
            raise DsdError(f"{path}: missing; members cannot be removed with pack")
        replacements[by_raw[entry["raw_name_hex"]].name] = path.read_bytes()
    contents = replacements.get(CONTENTS, src.contents)
    if contents.startswith(UTF8_BOM):
        raise DsdError("contents.xml gained a UTF-8 BOM; save it as UTF-8 without BOM")
    changed = sorted(k for k, v in replacements.items() if v != src.get(k).data)
    log = [f"repacking; changed member(s): {', '.join(changed) if changed else 'none'}"]
    return _finish_write(src, replacements, Path(args.output), args.force, log)


def build_parser() -> argparse.ArgumentParser:
    p = argparse.ArgumentParser(
        prog="dsd.py",
        description="Read, check, and surgically edit DART Editor .dsd documents.",
        epilog="Locators: tables are numbered T1.. in document order; rows R1.. count THEAD and TBODY rows;"
        " columns C1.. are grid columns after COLSPAN/ROWSPAN expansion.",
    )
    p.add_argument("--version", action="version", version=VERSION)
    sub = p.add_subparsers(dest="command", required=True)

    s = sub.add_parser("inspect", help="archive, metadata, structure counts, and issues (no document text)")
    s.add_argument("file")
    s.add_argument("--json", action="store_true")
    s.set_defaults(func=cmd_inspect)

    s = sub.add_parser("outline", help="sections, tables (T#), insertions, and images with contents.xml line numbers")
    s.add_argument("file")
    s.add_argument("--all", action="store_true", help="also list paragraphs, warnings, and empty insertions")
    s.set_defaults(func=cmd_outline)

    s = sub.add_parser(
        "read",
        help="list every block with its line, element path, and exact text; tables as T# stubs",
        description="One line per block: contents.xml line, element path, and the exact text as a JSON string"
        " (&cr; shown as ⏎). Tables appear as T# stubs; read their cells with `table`. Elements without"
        " a rule appear with their attributes, never hidden. Images stay in the archive (see `extract`).",
    )
    s.add_argument("file")
    s.add_argument("-o", "--output", help="write the view to this file instead of stdout")
    s.add_argument("--scaffold", action="store_true", help="also show editor scaffolding (COMMENT, LIBRARYLIST, FILENAME)")
    s.add_argument("--no-header", action="store_true", help="omit the leading # metadata lines")
    s.add_argument("--force", action="store_true", help="replace existing output")
    s.set_defaults(func=cmd_read)

    s = sub.add_parser("table", help="show tables cell by cell with R/C coordinates")
    s.add_argument("file")
    s.add_argument("tables", nargs="+", type=int, metavar="N")
    s.add_argument("--spans", action="store_true", help="list every grid position covered by a span")
    s.set_defaults(func=cmd_table)

    s = sub.add_parser("find", help="locate text in cells and paragraphs (whitespace-insensitive by default)")
    s.add_argument("file")
    s.add_argument("text")
    s.add_argument("--exact", action="store_true", help="match whitespace exactly")
    s.add_argument("--limit", type=int, default=50)
    s.set_defaults(func=cmd_find)

    s = sub.add_parser("check", help="structural checks; optional schema validation and comparison")
    s.add_argument("file")
    s.add_argument("--against", metavar="ORIGINAL", help="compare members and structure with the original .dsd")
    s.add_argument("--schema-dir", help="directory holding Dart3.xsd/Dart4.xsd (default: C:/DART/Resource/Dtd if present)")
    s.add_argument("--no-schema", action="store_true", help="skip schema validation")
    s.add_argument("--json", action="store_true")
    s.set_defaults(func=cmd_check)

    s = sub.add_parser("diff", help="list changed cells and paragraphs between two .dsd files")
    s.add_argument("original")
    s.add_argument("edited")
    s.add_argument("--limit", type=int, default=200)
    s.add_argument("--raw", action="store_true", help="also print the changed contents.xml lines")
    s.set_defaults(func=cmd_diff)

    def edit_common(sp: argparse.ArgumentParser) -> None:
        sp.add_argument("file")
        sp.add_argument("-o", "--output", required=True, help="new .dsd path (never the input unless --force)")
        sp.add_argument("--force", action="store_true", help="replace an existing output file")

    s = sub.add_parser("set-cell", help="replace one table cell's text")
    edit_common(s)
    s.add_argument("--table", type=int, required=True)
    s.add_argument("--row", type=int, required=True)
    s.add_argument("--col", type=int, required=True)
    s.add_argument("--text", required=True, help="new text; newlines become DART line breaks")
    s.add_argument("--unit-value", help="new AUNITVALUE for TU cells that carry one (e.g. 20251231)")
    s.add_argument("--flatten", action="store_true", help="replace mixed/multi-paragraph content with plain text")
    s.set_defaults(func=cmd_set_cell)

    s = sub.add_parser(
        "replace",
        help="replace literal text inside text nodes, guarded by an expected count",
        description="Matches document text within one text run (not across SPAN/P boundaries), never in tags,"
        " attribute values, comments, or editor scaffolding. A newline in --old/--new means a DART line break.",
    )
    s.add_argument("file")
    s.add_argument("-o", "--output", help="new .dsd path (never the input unless --force)")
    s.add_argument("--force", action="store_true", help="replace an existing output file")
    s.add_argument("--dry-run", action="store_true", help="list matches with context and write nothing")
    s.add_argument("--old", required=True)
    s.add_argument("--new", required=True)
    s.add_argument("--expect", default="1", help="required number of occurrences (default 1), or 'any'")
    s.add_argument("--table", type=int, help="limit to table N")
    s.set_defaults(func=cmd_replace)

    s = sub.add_parser("copy-row", help="duplicate a table row below itself")
    edit_common(s)
    s.add_argument("--table", type=int, required=True)
    s.add_argument("--row", type=int, required=True)
    s.add_argument("--count", type=int, default=1)
    s.add_argument("--clear", action="store_true", help="empty the copied cells' text")
    s.add_argument("--force-locked", action="store_true", help="ignore ACOPY=\"N\" (form-locked rows)")
    s.set_defaults(func=cmd_copy_row)

    s = sub.add_parser("delete-row", help="delete a table row")
    edit_common(s)
    s.add_argument("--table", type=int, required=True)
    s.add_argument("--row", type=int, required=True)
    s.add_argument("--force-locked", action="store_true", help="ignore ADELETE=\"N\" (form-locked rows)")
    s.set_defaults(func=cmd_delete_row)

    s = sub.add_parser(
        "apply",
        help="apply a JSON array of edits in order and write one new .dsd",
        formatter_class=argparse.RawDescriptionHelpFormatter,
        epilog="Edit objects (required fields first; later edits see earlier edits' row numbers):\n"
        + "\n".join(
            f"  {kind:10} " + ", ".join(f"{k}{'' if req else '?'}: {t.__name__ if t is not object else 'int|any'}"
                                         for k, (t, req) in spec.items())
            for kind, spec in OP_FIELDS.items()
        )
        + '\nExample: [{"op": "set-cell", "table": 3, "row": 4, "col": 2, "text": "1,100,000"}]',
    )
    edit_common(s)
    s.add_argument("edits", help="JSON file: [{\"op\": \"set-cell\"|\"replace\"|\"copy-row\"|\"delete-row\", ...}]")
    s.set_defaults(func=cmd_apply)

    s = sub.add_parser("extract", help="unpack members for manual XML editing (with a repack manifest)")
    s.add_argument("file")
    s.add_argument("directory")
    s.add_argument("--force", action="store_true")
    s.set_defaults(func=cmd_extract)

    s = sub.add_parser("pack", help="repack an extracted directory using the original archive's layout")
    s.add_argument("directory")
    s.add_argument("original", help="the .dsd the directory was extracted from")
    s.add_argument("-o", "--output", required=True)
    s.add_argument("--force", action="store_true")
    s.set_defaults(func=cmd_pack)
    return p


VALUE_FLAGS = ("--text", "--old", "--new", "--unit-value")


def _join_value_flags(argv: List[str]) -> List[str]:
    """Let text values start with '-' (e.g. --text -1,000) by binding them as --flag=value."""
    out: List[str] = []
    i = 0
    while i < len(argv):
        if argv[i] in VALUE_FLAGS and i + 1 < len(argv):
            out.append(f"{argv[i]}={argv[i + 1]}")
            i += 2
        else:
            out.append(argv[i])
            i += 1
    return out


def main(argv: Optional[Sequence[str]] = None) -> int:
    for stream in (sys.stdout, sys.stderr):
        try:
            stream.reconfigure(encoding="utf-8")  # type: ignore[attr-defined]
        except (AttributeError, ValueError):
            pass
    args = build_parser().parse_args(_join_value_flags(sys.argv[1:] if argv is None else list(argv)))
    try:
        return int(args.func(args))
    except DsdError as exc:
        sys.stderr.write(f"error: {exc}\n")
        return 1


if __name__ == "__main__":
    sys.exit(main())
