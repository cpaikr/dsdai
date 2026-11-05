//! A positioned, non-validating XML parser.
//!
//! Edits splice exact byte ranges of `contents.xml`, so every element records
//! where its start tag, content, and end tag lie in the source bytes. The tree
//! keeps decoded character data the way an expat-based reader sees it: line
//! endings normalized to LF, references resolved, and comments, processing
//! instructions, and CDATA delimiters dropped. DOCTYPE and entity declarations
//! are refused so no document can expand or fetch external content.

use crate::error::{Error, ErrorCode, Result};

pub type NodeId = usize;

/// Deeper nesting is refused: no DART document comes close, and every
/// consumer of the tree recurses over it.
pub const MAX_DEPTH: usize = 512;

/// The `encoding` pseudo-attribute of an XML declaration body.
fn declared_encoding(decl: &str) -> Option<&str> {
    let at = decl.find("encoding")?;
    let rest = decl[at + 8..].trim_start().strip_prefix('=')?.trim_start();
    let quote = rest.chars().next().filter(|q| *q == '"' || *q == '\'')?;
    let body = &rest[1..];
    body.find(quote).map(|end| &body[..end])
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Item {
    Text(String),
    Node(NodeId),
}

#[derive(Debug, Clone)]
pub struct Node {
    pub tag: String,
    /// Attributes in document order, values normalized as XML requires.
    pub attrs: Vec<(String, String)>,
    /// Offset of `<`.
    pub start: usize,
    /// Offset just after the start tag's `>`.
    pub open_end: usize,
    /// Offset of `</`; equals `open_end` for an empty-element tag.
    pub close_start: usize,
    /// Offset just after the element.
    pub end: usize,
    pub parent: Option<NodeId>,
    pub items: Vec<Item>,
    pub children: Vec<NodeId>,
    /// Written as `<X ... />` rather than `<X ...></X>`.
    pub empty_tag: bool,
}

impl Node {
    pub fn attr(&self, name: &str) -> Option<&str> {
        self.attrs.iter().find(|(k, _)| k == name).map(|(_, v)| v.as_str())
    }

    pub fn has_attr(&self, name: &str) -> bool {
        self.attrs.iter().any(|(k, _)| k == name)
    }
}

#[derive(Debug, Clone)]
pub struct Tree {
    pub nodes: Vec<Node>,
    pub root: NodeId,
}

impl Tree {
    pub fn node(&self, id: NodeId) -> &Node {
        &self.nodes[id]
    }

    pub fn tag(&self, id: NodeId) -> &str {
        &self.nodes[id].tag
    }

    pub fn children(&self, id: NodeId) -> &[NodeId] {
        &self.nodes[id].children
    }

    /// The subtree in document order, `id` first.
    pub fn descendants(&self, id: NodeId) -> Vec<NodeId> {
        let mut out = Vec::new();
        let mut stack = vec![id];
        while let Some(n) = stack.pop() {
            out.push(n);
            stack.extend(self.nodes[n].children.iter().rev());
        }
        out
    }

    /// Nodes of the subtree with this tag, in document order.
    pub fn iter_tag(&self, id: NodeId, tag: &str) -> Vec<NodeId> {
        self.descendants(id).into_iter().filter(|&n| self.nodes[n].tag == tag).collect()
    }

    pub fn first_tag(&self, id: NodeId, tag: &str) -> Option<NodeId> {
        self.descendants(id).into_iter().find(|&n| self.nodes[n].tag == tag)
    }

    pub fn ancestors(&self, id: NodeId) -> Vec<NodeId> {
        let mut out = Vec::new();
        let mut cur = self.nodes[id].parent;
        while let Some(p) = cur {
            out.push(p);
            cur = self.nodes[p].parent;
        }
        out
    }

    pub fn child_tag(&self, id: NodeId, tag: &str) -> Option<NodeId> {
        self.nodes[id].children.iter().copied().find(|&c| self.nodes[c].tag == tag)
    }
}

fn is_space(b: u8) -> bool {
    matches!(b, b' ' | b'\t' | b'\r' | b'\n')
}

fn is_name_start(c: char) -> bool {
    matches!(c,
        ':' | 'A'..='Z' | '_' | 'a'..='z'
        | '\u{C0}'..='\u{D6}' | '\u{D8}'..='\u{F6}' | '\u{F8}'..='\u{2FF}'
        | '\u{370}'..='\u{37D}' | '\u{37F}'..='\u{1FFF}' | '\u{200C}'..='\u{200D}'
        | '\u{2070}'..='\u{218F}' | '\u{2C00}'..='\u{2FEF}' | '\u{3001}'..='\u{D7FF}'
        | '\u{F900}'..='\u{FDCF}' | '\u{FDF0}'..='\u{FFFD}' | '\u{10000}'..='\u{EFFFF}')
}

fn is_name_char(c: char) -> bool {
    is_name_start(c) || matches!(c, '-' | '.' | '0'..='9' | '\u{B7}' | '\u{300}'..='\u{36F}' | '\u{203F}'..='\u{2040}')
}

fn is_xml_char(c: char) -> bool {
    matches!(c, '\t' | '\n' | '\r' | '\u{20}'..='\u{D7FF}' | '\u{E000}'..='\u{FFFD}' | '\u{10000}'..='\u{10FFFF}')
}

struct Parser<'a> {
    src: &'a str,
    data: &'a [u8],
    pos: usize,
    nodes: Vec<Node>,
}

impl Parser<'_> {
    fn err(&self, at: usize, what: &str) -> Error {
        let line = self.data[..at.min(self.data.len())].iter().filter(|&&b| b == b'\n').count() + 1;
        let line_start = self.data[..at.min(self.data.len())].iter().rposition(|&b| b == b'\n').map_or(0, |p| p + 1);
        let column = self.src.get(line_start..at).map_or(0, |s| s.chars().count());
        Error::new(ErrorCode::InvalidXml, format!("XML is not well-formed: {what}: line {line}, column {column}"))
    }

    fn starts_with(&self, s: &str) -> bool {
        self.data[self.pos..].starts_with(s.as_bytes())
    }

    fn skip_space(&mut self) -> usize {
        let from = self.pos;
        while self.pos < self.data.len() && is_space(self.data[self.pos]) {
            self.pos += 1;
        }
        self.pos - from
    }

    fn peek_char(&self) -> Option<char> {
        self.src[self.pos..].chars().next()
    }

    fn name(&mut self) -> Result<String> {
        let from = self.pos;
        match self.peek_char() {
            Some(c) if is_name_start(c) => self.pos += c.len_utf8(),
            _ => return Err(self.err(from, "not well-formed (invalid token)")),
        }
        while let Some(c) = self.peek_char() {
            if !is_name_char(c) {
                break;
            }
            self.pos += c.len_utf8();
        }
        Ok(self.src[from..self.pos].to_owned())
    }

    fn find(&self, needle: &str, from: usize) -> Option<usize> {
        let hay = &self.data[from..];
        hay.windows(needle.len()).position(|w| w == needle.as_bytes()).map(|p| from + p)
    }

    /// Resolve a reference starting at `&`; returns the character.
    fn reference(&mut self) -> Result<char> {
        let at = self.pos;
        let semi = self.data[at..].iter().take(40).position(|&b| b == b';').map(|p| at + p);
        let Some(semi) = semi else {
            return Err(self.err(at, "not well-formed (invalid token)"));
        };
        let body = &self.src[at + 1..semi];
        let ch = if let Some(num) = body.strip_prefix('#') {
            let value = if let Some(hex) = num.strip_prefix('x') {
                u32::from_str_radix(hex, 16)
                    .ok()
                    .filter(|_| !hex.is_empty() && hex.bytes().all(|b| b.is_ascii_hexdigit()))
            } else {
                num.parse::<u32>().ok().filter(|_| !num.is_empty() && num.bytes().all(|b| b.is_ascii_digit()))
            };
            match value.and_then(char::from_u32) {
                Some(c) if is_xml_char(c) => c,
                _ => return Err(self.err(at, "reference to invalid character number")),
            }
        } else {
            match body {
                "amp" => '&',
                "lt" => '<',
                "gt" => '>',
                "quot" => '"',
                "apos" => '\'',
                _ => return Err(self.err(at, "undefined entity")),
            }
        };
        self.pos = semi + 1;
        Ok(ch)
    }

    /// A comment, processing instruction, or declaration at `<!`/`<?`.
    fn skip_markup(&mut self, in_prolog: bool) -> Result<()> {
        let at = self.pos;
        if self.starts_with("<!--") {
            let end = self.find("--", at + 4).ok_or_else(|| self.err(at, "unclosed token"))?;
            if self.data.get(end + 2) != Some(&b'>') {
                return Err(self.err(end, "not well-formed (invalid token)"));
            }
            self.check_chars(at + 4, end)?;
            self.pos = end + 3;
        } else if self.starts_with("<?") {
            self.pos += 2;
            let target = self.name()?;
            if target.eq_ignore_ascii_case("xml") && !(in_prolog && at == self.prolog_start()) {
                return Err(self.err(at, "XML or text declaration not at start of entity"));
            }
            let end = self.find("?>", self.pos).ok_or_else(|| self.err(at, "unclosed token"))?;
            self.check_chars(self.pos, end)?;
            if target == "xml"
                && let Some(enc) = declared_encoding(&self.src[self.pos..end])
                && !enc.eq_ignore_ascii_case("utf-8")
                && !enc.eq_ignore_ascii_case("utf8")
            {
                return Err(Error::new(
                    ErrorCode::InvalidXml,
                    format!("XML declares encoding {enc}; .dsd XML must be UTF-8"),
                ));
            }
            self.pos = end + 2;
        } else if self.starts_with("<!DOCTYPE") || self.starts_with("<!ENTITY") {
            return Err(Error::new(ErrorCode::InvalidXml, "DOCTYPE/entity declarations are not allowed in .dsd XML"));
        } else {
            return Err(self.err(at, "not well-formed (invalid token)"));
        }
        Ok(())
    }

    fn prolog_start(&self) -> usize {
        if self.data.starts_with(b"\xef\xbb\xbf") { 3 } else { 0 }
    }

    fn check_chars(&self, from: usize, to: usize) -> Result<()> {
        let mut at = from;
        for c in self.src[from..to].chars() {
            if !is_xml_char(c) {
                return Err(self.err(at, "not well-formed (invalid token)"));
            }
            at += c.len_utf8();
        }
        Ok(())
    }

    fn misc(&mut self, in_prolog: bool) -> Result<()> {
        loop {
            self.skip_space();
            if self.starts_with("<!") || self.starts_with("<?") {
                self.skip_markup(in_prolog)?;
            } else {
                return Ok(());
            }
        }
    }

    fn start_tag(&mut self, parent: Option<NodeId>) -> Result<NodeId> {
        let start = self.pos;
        self.pos += 1;
        let tag = self.name()?;
        let mut attrs: Vec<(String, String)> = Vec::new();
        let empty_tag = loop {
            let spaced = self.skip_space() > 0;
            if self.starts_with("/>") {
                self.pos += 2;
                break true;
            }
            if self.starts_with(">") {
                self.pos += 1;
                break false;
            }
            if !spaced {
                return Err(self.err(self.pos, "not well-formed (invalid token)"));
            }
            let name_at = self.pos;
            let name = self.name()?;
            self.skip_space();
            if !self.starts_with("=") {
                return Err(self.err(self.pos, "not well-formed (invalid token)"));
            }
            self.pos += 1;
            self.skip_space();
            let value = self.attr_value()?;
            if attrs.iter().any(|(k, _)| *k == name) {
                return Err(self.err(name_at, "duplicate attribute"));
            }
            attrs.push((name, value));
        };
        let id = self.nodes.len();
        self.nodes.push(Node {
            tag,
            attrs,
            start,
            open_end: self.pos,
            close_start: if empty_tag { self.pos } else { 0 },
            end: if empty_tag { self.pos } else { 0 },
            parent,
            items: Vec::new(),
            children: Vec::new(),
            empty_tag,
        });
        if let Some(p) = parent {
            self.nodes[p].items.push(Item::Node(id));
            self.nodes[p].children.push(id);
        }
        Ok(id)
    }

    fn attr_value(&mut self) -> Result<String> {
        let Some(&quote @ (b'"' | b'\'')) = self.data.get(self.pos) else {
            return Err(self.err(self.pos, "not well-formed (invalid token)"));
        };
        self.pos += 1;
        let mut value = String::new();
        loop {
            let Some(c) = self.peek_char() else {
                return Err(self.err(self.pos, "unclosed token"));
            };
            match c {
                _ if c as u32 == u32::from(quote) => {
                    self.pos += 1;
                    return Ok(value);
                }
                '<' => return Err(self.err(self.pos, "not well-formed (invalid token)")),
                '&' => value.push(self.reference()?),
                '\r' => {
                    self.pos += 1;
                    if self.data.get(self.pos) == Some(&b'\n') {
                        self.pos += 1;
                    }
                    value.push(' ');
                }
                '\n' | '\t' => {
                    self.pos += 1;
                    value.push(' ');
                }
                _ if !is_xml_char(c) => return Err(self.err(self.pos, "not well-formed (invalid token)")),
                _ => {
                    self.pos += c.len_utf8();
                    value.push(c);
                }
            }
        }
    }

    fn push_text(&mut self, node: NodeId, text: &str) {
        if text.is_empty() {
            return;
        }
        match self.nodes[node].items.last_mut() {
            Some(Item::Text(prev)) => prev.push_str(text),
            _ => self.nodes[node].items.push(Item::Text(text.to_owned())),
        }
    }

    fn content(&mut self, root: NodeId) -> Result<()> {
        let mut stack = vec![root];
        let mut text = String::new();
        while let Some(&top) = stack.last() {
            let Some(c) = self.peek_char() else {
                return Err(self.err(self.pos, "no element found"));
            };
            match c {
                '<' => {
                    if self.starts_with("<![CDATA[") {
                        let at = self.pos;
                        let end = self.find("]]>", at + 9).ok_or_else(|| self.err(at, "unclosed CDATA section"))?;
                        self.check_chars(at + 9, end)?;
                        text.push_str(&self.src[at + 9..end].replace("\r\n", "\n").replace('\r', "\n"));
                        self.pos = end + 3;
                        continue;
                    }
                    if self.starts_with("<!") || self.starts_with("<?") {
                        self.skip_markup(false)?;
                        continue;
                    }
                    self.push_text(top, &std::mem::take(&mut text));
                    if self.starts_with("</") {
                        let close_start = self.pos;
                        self.pos += 2;
                        let name = self.name()?;
                        self.skip_space();
                        if !self.starts_with(">") || name != self.nodes[top].tag {
                            return Err(self.err(close_start, "mismatched tag"));
                        }
                        self.pos += 1;
                        self.nodes[top].close_start = close_start;
                        self.nodes[top].end = self.pos;
                        stack.pop();
                    } else {
                        let id = self.start_tag(Some(top))?;
                        if !self.nodes[id].empty_tag {
                            if stack.len() >= MAX_DEPTH {
                                return Err(self.err(self.nodes[id].start, "elements are nested too deeply"));
                            }
                            stack.push(id);
                        }
                    }
                }
                '&' => text.push(self.reference()?),
                '\r' => {
                    self.pos += 1;
                    if self.data.get(self.pos) == Some(&b'\n') {
                        self.pos += 1;
                    }
                    text.push('\n');
                }
                ']' if self.starts_with("]]>") => return Err(self.err(self.pos, "not well-formed (invalid token)")),
                _ if !is_xml_char(c) => return Err(self.err(self.pos, "not well-formed (invalid token)")),
                _ => {
                    self.pos += c.len_utf8();
                    text.push(c);
                }
            }
        }
        Ok(())
    }
}

/// Parse `data` as a UTF-8 XML document.
pub fn parse(data: &[u8]) -> Result<Tree> {
    let src = std::str::from_utf8(data).map_err(|e| {
        Error::new(ErrorCode::InvalidXml, format!("XML is not well-formed: invalid UTF-8 at byte {}", e.valid_up_to()))
    })?;
    let mut p = Parser { src, data, pos: 0, nodes: Vec::new() };
    p.pos = p.prolog_start();
    p.misc(true)?;
    if !p.starts_with("<") || p.starts_with("</") {
        return Err(if p.pos >= data.len() {
            Error::new(ErrorCode::InvalidXml, "XML has no root element")
        } else {
            p.err(p.pos, "syntax error")
        });
    }
    let root = p.start_tag(None)?;
    if !p.nodes[root].empty_tag {
        p.content(root)?;
    }
    p.misc(false)?;
    if p.pos < data.len() {
        return Err(p.err(p.pos, "junk after document element"));
    }
    Ok(Tree { nodes: p.nodes, root })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn records_byte_positions() {
        let src = "<?xml version=\"1.0\"?>\r\n<A X=\"1\"><B/>t&amp;u<C>v</C></A>";
        let tree = parse(src.as_bytes()).unwrap();
        let a = tree.node(tree.root);
        assert_eq!(&src[a.start..a.open_end], "<A X=\"1\">");
        assert_eq!(&src[a.close_start..a.end], "</A>");
        let b = tree.node(a.children[0]);
        assert!(b.empty_tag);
        assert_eq!((b.close_start, b.end), (b.open_end, b.open_end));
        assert_eq!(a.items[1], Item::Text("t&u".into()));
        assert_eq!(a.attr("X"), Some("1"));
    }

    #[test]
    fn normalizes_text_and_attributes() {
        let tree = parse(b"<A V=\"a\r\nb\tc&#10;\">x\r\ny\rz<!-- c --><![CDATA[<q>]]></A>").unwrap();
        let a = tree.node(tree.root);
        assert_eq!(a.attr("V"), Some("a b c\n"));
        assert_eq!(a.items, vec![Item::Text("x\ny\nz<q>".into())]);
    }

    #[test]
    fn refuses_unsafe_or_malformed_input() {
        for bad in [
            &b"<!DOCTYPE a [<!ENTITY x 'y'>]><a/>"[..],
            b"<a>&nbsp;</a>",
            b"<a></b>",
            b"<a x='1' x='2'/>",
            b"<a/><b/>",
            b"<a>\x01</a>",
            b"",
        ] {
            assert!(parse(bad).is_err(), "{:?}", String::from_utf8_lossy(bad));
        }
    }
}
