//! Validation against a locally installed DART schema.
//!
//! The DART schemas use a small part of XML Schema: global and local element
//! declarations, named and anonymous complex types (element-only, mixed,
//! empty, or simple content), sequences, choices, model groups, occurrence
//! bounds, and attributes typed as strings, NMTOKENs, IDs, or enumerations.
//! This module implements exactly that subset and reports any other
//! construct as unsupported instead of guessing at its meaning. No schema
//! text ships with the tool; it reads the user's installed copy.

use std::collections::{BTreeMap, HashMap, HashSet};
use std::path::{Path, PathBuf};
use std::rc::Rc;

use crate::check::Issue;
use crate::doc::Doc;
use crate::error::{Error, Result};
use crate::text::header_info;
use crate::xml::{self, Item, NodeId, Tree};

const XS_NS: &str = "http://www.w3.org/2001/XMLSchema";
const DEFAULT_SCHEMA_DIR: &str = "C:/DART/Resource/Dtd";
const MAX_REPORTED: usize = 20;

#[derive(Debug, Clone)]
enum Simple {
    String,
    NmToken,
    Id,
    Enum { base: Box<Simple>, values: Vec<String> },
}

#[derive(Debug, Clone, Copy)]
enum Occurs {
    Bounded(usize),
    Unbounded,
}

#[derive(Debug, Clone)]
enum Particle {
    Element { name: String, ty: TypeId, min: usize, max: Occurs },
    Sequence { items: Vec<Particle>, min: usize, max: Occurs },
    Choice { items: Vec<Particle>, min: usize, max: Occurs },
}

#[derive(Debug, Clone)]
enum Content {
    Empty,
    Model(Particle),
    Simple(Simple),
    /// No type given: anything goes.
    Any,
}

#[derive(Debug, Clone)]
struct Attr {
    name: String,
    ty: Simple,
    required: bool,
}

#[derive(Debug, Clone)]
struct Complex {
    mixed: bool,
    content: Content,
    attrs: Vec<Attr>,
}

type TypeId = usize;

/// A compiled schema.
#[derive(Debug, Clone)]
pub struct Schema {
    types: Vec<Complex>,
    globals: HashMap<String, TypeId>,
}

fn unsupported(what: &str) -> Error {
    Error::invalid(format!("unsupported schema construct: {what}"))
}

struct Builder<'a> {
    tree: &'a Tree,
    prefix: String,
    named_complex: HashMap<String, NodeId>,
    named_simple: HashMap<String, NodeId>,
    groups: HashMap<String, NodeId>,
    elements: HashMap<String, NodeId>,
    types: Vec<Option<Complex>>,
    memo: HashMap<String, TypeId>,
    group_stack: Vec<String>,
}

impl Builder<'_> {
    fn local<'b>(&self, tag: &'b str) -> Option<&'b str> {
        tag.strip_prefix(self.prefix.as_str())
    }

    fn kind(&self, node: NodeId) -> &str {
        self.local(self.tree.tag(node)).unwrap_or("")
    }

    fn attr(&self, node: NodeId, name: &str) -> Option<&str> {
        self.tree.node(node).attr(name)
    }

    /// Element children other than annotations.
    fn parts(&self, node: NodeId) -> Vec<NodeId> {
        self.tree.children(node).iter().copied().filter(|&c| self.kind(c) != "annotation").collect()
    }

    fn check_attrs(&self, node: NodeId, allowed: &[&str]) -> Result<()> {
        for (k, _) in &self.tree.node(node).attrs {
            if !allowed.contains(&k.as_str()) && !k.starts_with("xmlns") {
                return Err(unsupported(&format!("attribute {k} on {}", self.kind(node))));
            }
        }
        Ok(())
    }

    fn occurs(&self, node: NodeId) -> Result<(usize, Occurs)> {
        let min = match self.attr(node, "minOccurs") {
            None => 1,
            Some(v) => v.trim().parse().map_err(|_| unsupported(&format!("minOccurs={v}")))?,
        };
        let max = match self.attr(node, "maxOccurs") {
            None => Occurs::Bounded(1),
            Some("unbounded") => Occurs::Unbounded,
            Some(v) => Occurs::Bounded(v.trim().parse().map_err(|_| unsupported(&format!("maxOccurs={v}")))?),
        };
        Ok((min, max))
    }

    fn builtin(&self, qname: &str) -> Option<Simple> {
        match qname.strip_prefix(self.prefix.as_str())? {
            "string" => Some(Simple::String),
            "NMTOKEN" => Some(Simple::NmToken),
            "ID" => Some(Simple::Id),
            _ => None,
        }
    }

    fn simple_by_name(&self, qname: &str) -> Result<Simple> {
        if let Some(s) = self.builtin(qname) {
            return Ok(s);
        }
        if qname.starts_with(self.prefix.as_str()) {
            return Err(unsupported(&format!("built-in type {qname}")));
        }
        let node = *self.named_simple.get(qname).ok_or_else(|| unsupported(&format!("unknown simple type {qname}")))?;
        self.simple_type(node)
    }

    fn simple_type(&self, node: NodeId) -> Result<Simple> {
        self.check_attrs(node, &["name"])?;
        let parts = self.parts(node);
        let [restriction] = parts.as_slice() else {
            return Err(unsupported("simpleType without a single restriction"));
        };
        if self.kind(*restriction) != "restriction" {
            return Err(unsupported(&format!("simpleType {}", self.kind(*restriction))));
        }
        self.check_attrs(*restriction, &["base"])?;
        let base = self.simple_by_name(self.attr(*restriction, "base").unwrap_or(""))?;
        let mut values = Vec::new();
        for facet in self.parts(*restriction) {
            if self.kind(facet) != "enumeration" {
                return Err(unsupported(&format!("facet {}", self.kind(facet))));
            }
            values.push(self.attr(facet, "value").unwrap_or("").to_owned());
        }
        Ok(if values.is_empty() { base } else { Simple::Enum { base: Box::new(base), values } })
    }

    fn attribute(&self, node: NodeId) -> Result<Attr> {
        self.check_attrs(node, &["name", "type", "use", "default"])?;
        let name = self.attr(node, "name").ok_or_else(|| unsupported("attribute without a name"))?.to_owned();
        let inline = self.parts(node);
        let ty = match (self.attr(node, "type"), inline.as_slice()) {
            (Some(t), []) => self.simple_by_name(t)?,
            (None, [s]) if self.kind(*s) == "simpleType" => self.simple_type(*s)?,
            (None, []) => Simple::String,
            _ => return Err(unsupported(&format!("attribute {name} type"))),
        };
        let required = match self.attr(node, "use") {
            None | Some("optional") => false,
            Some("required") => true,
            Some(other) => return Err(unsupported(&format!("use={other}"))),
        };
        Ok(Attr { name, ty, required })
    }

    fn type_by_name(&mut self, qname: &str) -> Result<TypeId> {
        if let Some(&id) = self.memo.get(qname) {
            return Ok(id);
        }
        if let Some(simple) =
            self.builtin(qname).map(Ok).or_else(|| self.named_simple.get(qname).copied().map(|n| self.simple_type(n)))
        {
            let id = self.push(Complex { mixed: false, content: Content::Simple(simple?), attrs: Vec::new() });
            self.memo.insert(qname.to_owned(), id);
            return Ok(id);
        }
        let node = *self.named_complex.get(qname).ok_or_else(|| unsupported(&format!("unknown type {qname}")))?;
        let id = self.types.len();
        self.types.push(None);
        self.memo.insert(qname.to_owned(), id);
        // A reference back to an enclosing group through a named type is not
        // recursion: the type is already reserved, so compilation stops there.
        let outer = std::mem::take(&mut self.group_stack);
        let built = self.complex_type(node);
        self.group_stack = outer;
        self.types[id] = Some(built?);
        Ok(id)
    }

    fn push(&mut self, ty: Complex) -> TypeId {
        self.types.push(Some(ty));
        self.types.len() - 1
    }

    fn complex_type(&mut self, node: NodeId) -> Result<Complex> {
        self.check_attrs(node, &["name", "mixed"])?;
        let mixed = matches!(self.attr(node, "mixed"), Some("true" | "1"));
        let mut content = Content::Empty;
        let mut attrs = Vec::new();
        for part in self.parts(node) {
            match self.kind(part) {
                "sequence" | "choice" | "group" if matches!(content, Content::Empty) => {
                    content = Content::Model(self.particle(part)?);
                }
                "simpleContent" => {
                    let inner = self.parts(part);
                    let [ext] = inner.as_slice() else { return Err(unsupported("simpleContent shape")) };
                    if self.kind(*ext) != "extension" {
                        return Err(unsupported(&format!("simpleContent {}", self.kind(*ext))));
                    }
                    self.check_attrs(*ext, &["base"])?;
                    content = Content::Simple(self.simple_by_name(self.attr(*ext, "base").unwrap_or(""))?);
                    for a in self.parts(*ext) {
                        if self.kind(a) != "attribute" {
                            return Err(unsupported(&format!("{} in an extension", self.kind(a))));
                        }
                        attrs.push(self.attribute(a)?);
                    }
                }
                "attribute" => attrs.push(self.attribute(part)?),
                other => return Err(unsupported(&format!("{other} in complexType"))),
            }
        }
        Ok(Complex { mixed, content, attrs })
    }

    fn particle(&mut self, node: NodeId) -> Result<Particle> {
        let (min, max) = self.occurs(node)?;
        let kind = self.kind(node).to_owned();
        match kind.as_str() {
            "element" => {
                self.check_attrs(node, &["name", "type", "ref", "minOccurs", "maxOccurs"])?;
                let (name, ty) = if let Some(r) = self.attr(node, "ref") {
                    let r = r.to_owned();
                    let ty = self.global_element(&r)?;
                    (r, ty)
                } else {
                    let name = self.attr(node, "name").ok_or_else(|| unsupported("element without a name"))?.to_owned();
                    (name, self.element_type(node)?)
                };
                Ok(Particle::Element { name, ty, min, max })
            }
            kind @ ("sequence" | "choice") => {
                self.check_attrs(node, &["minOccurs", "maxOccurs"])?;
                let items = self.parts(node).into_iter().map(|c| self.particle(c)).collect::<Result<Vec<_>>>()?;
                Ok(if kind == "sequence" {
                    Particle::Sequence { items, min, max }
                } else {
                    Particle::Choice { items, min, max }
                })
            }
            "group" => {
                self.check_attrs(node, &["ref", "minOccurs", "maxOccurs"])?;
                let r = self.attr(node, "ref").ok_or_else(|| unsupported("group without ref"))?.to_owned();
                if self.group_stack.contains(&r) {
                    return Err(unsupported(&format!("recursive group {r}")));
                }
                let def = *self.groups.get(&r).ok_or_else(|| unsupported(&format!("unknown group {r}")))?;
                let inner = self.parts(def);
                let [body] = inner.as_slice() else { return Err(unsupported(&format!("group {r} shape"))) };
                self.group_stack.push(r);
                let items = vec![self.particle(*body)?];
                self.group_stack.pop();
                Ok(Particle::Sequence { items, min, max })
            }
            other => Err(unsupported(&format!("{other} in a content model"))),
        }
    }

    /// A global element's type, compiled once so recursive references terminate.
    fn global_element(&mut self, name: &str) -> Result<TypeId> {
        let key = format!("@{name}");
        if let Some(&id) = self.memo.get(&key) {
            return Ok(id);
        }
        let node = *self.elements.get(name).ok_or_else(|| unsupported(&format!("unknown element ref {name}")))?;
        self.check_attrs(node, &["name", "type"])?;
        let inline = self.parts(node);
        if let (None, [c]) = (self.attr(node, "type"), inline.as_slice())
            && self.kind(*c) == "complexType"
        {
            let id = self.types.len();
            self.types.push(None);
            self.memo.insert(key, id);
            let outer = std::mem::take(&mut self.group_stack);
            let built = self.complex_type(*c);
            self.group_stack = outer;
            self.types[id] = Some(built?);
            return Ok(id);
        }
        let id = self.element_type(node)?;
        self.memo.insert(key, id);
        Ok(id)
    }

    fn element_type(&mut self, node: NodeId) -> Result<TypeId> {
        let inline = self.parts(node);
        match (self.attr(node, "type").map(str::to_owned), inline.as_slice()) {
            (Some(t), []) => self.type_by_name(&t),
            (None, [c]) if self.kind(*c) == "complexType" => {
                let built = self.complex_type(*c)?;
                Ok(self.push(built))
            }
            (None, [s]) if self.kind(*s) == "simpleType" => {
                let simple = self.simple_type(*s)?;
                Ok(self.push(Complex { mixed: false, content: Content::Simple(simple), attrs: Vec::new() }))
            }
            (None, []) => Ok(self.push(Complex { mixed: true, content: Content::Any, attrs: Vec::new() })),
            _ => Err(unsupported("element type")),
        }
    }
}

impl Schema {
    /// Compile an XSD document of the supported subset.
    pub fn compile(data: &[u8]) -> Result<Self> {
        let tree = xml::parse(data).map_err(|e| Error::invalid(format!("schema: {}", e.message)))?;
        let root = tree.node(tree.root);
        let prefix = match root.tag.split_once(':') {
            Some((p, "schema")) if root.attr(&format!("xmlns:{p}")) == Some(XS_NS) => format!("{p}:"),
            // Without a prefix, built-in and schema-defined type names could collide.
            None if root.tag == "schema" => return Err(unsupported("schema without a namespace prefix")),
            _ => return Err(unsupported("document is not an XML Schema")),
        };
        let mut b = Builder {
            tree: &tree,
            prefix,
            named_complex: HashMap::new(),
            named_simple: HashMap::new(),
            groups: HashMap::new(),
            elements: HashMap::new(),
            types: Vec::new(),
            memo: HashMap::new(),
            group_stack: Vec::new(),
        };
        for (k, _) in &root.attrs {
            if !k.starts_with("xmlns") && k != "elementFormDefault" && k != "attributeFormDefault" {
                return Err(unsupported(&format!("schema attribute {k}")));
            }
        }
        for c in b.parts(tree.root) {
            let name = b.attr(c, "name").unwrap_or("").to_owned();
            match b.kind(c) {
                "complexType" => b.named_complex.insert(name, c),
                "simpleType" => b.named_simple.insert(name, c),
                "group" => b.groups.insert(name, c),
                "element" => b.elements.insert(name, c),
                other => return Err(unsupported(&format!("top-level {other}"))),
            };
        }
        let mut globals = HashMap::new();
        let mut names: Vec<String> = b.elements.keys().cloned().collect();
        names.sort();
        for name in names {
            let ty = b.global_element(&name)?;
            globals.insert(name, ty);
        }
        // Named types nothing refers to are still compiled, so unsupported
        // constructs anywhere in the schema surface the same way.
        let mut complex: Vec<String> = b.named_complex.keys().cloned().collect();
        complex.sort();
        for name in complex {
            b.type_by_name(&name)?;
        }
        let types = b.types.into_iter().map(|t| t.expect("every reserved type is built")).collect();
        Ok(Self { types, globals })
    }

    /// Validate a document; returns error messages (with line numbers).
    pub fn validate(&self, doc: &Doc) -> Vec<String> {
        let mut v = Validator { schema: self, doc, errors: Vec::new(), ids: HashSet::new() };
        let root = doc.root();
        match self.globals.get(doc.tag(root)) {
            Some(&ty) => v.element(root, ty),
            None => v.error(root, &format!("<{}> is not declared as a document element", doc.tag(root))),
        }
        v.errors
    }
}

struct Validator<'a> {
    schema: &'a Schema,
    doc: &'a Doc,
    errors: Vec<String>,
    ids: HashSet<String>,
}

fn collapse(v: &str) -> String {
    v.split([' ', '\t', '\r', '\n']).filter(|s| !s.is_empty()).collect::<Vec<_>>().join(" ")
}

fn is_name_char(c: char) -> bool {
    c.is_alphanumeric() || matches!(c, '-' | '.' | '_' | ':' | '\u{B7}')
}

fn is_ncname(v: &str) -> bool {
    let mut chars = v.chars();
    chars.next().is_some_and(|c| c.is_alphabetic() || c == '_') && v.chars().all(|c| is_name_char(c) && c != ':')
}

/// The declarations assigned to the children matched so far, newest first,
/// shared between alternatives so extending one costs O(1).
#[derive(Clone, Default)]
struct Assign(Option<Rc<Link>>);

struct Link {
    ty: TypeId,
    prev: Assign,
}

impl Drop for Assign {
    // A chain as long as a run of siblings would otherwise be dropped recursively.
    fn drop(&mut self) {
        let mut next = self.0.take();
        while let Some(rc) = next {
            match Rc::try_unwrap(rc) {
                Ok(mut link) => next = link.prev.0.take(),
                Err(_) => break,
            }
        }
    }
}

impl Assign {
    fn push(&self, ty: TypeId) -> Self {
        Self(Some(Rc::new(Link { ty, prev: self.clone() })))
    }

    fn to_vec(&self) -> Vec<TypeId> {
        let mut out = Vec::new();
        let mut cur = self.0.as_ref();
        while let Some(link) = cur {
            out.push(link.ty);
            cur = link.prev.0.as_ref();
        }
        out.reverse();
        out
    }
}

impl Validator<'_> {
    fn error(&mut self, node: NodeId, message: &str) {
        let line = self.doc.line(self.doc.tree.node(node).start);
        self.errors.push(format!("line {line}: {message}"));
    }

    /// Why `value` is not valid for `ty`, if it is not.
    fn simple_problem(&mut self, ty: &Simple, value: &str) -> Option<String> {
        match ty {
            Simple::String => None,
            Simple::NmToken => {
                let v = collapse(value);
                (v.is_empty() || !v.chars().all(is_name_char)).then(|| format!("{value:?} is not a valid NMTOKEN"))
            }
            Simple::Id => {
                let v = collapse(value);
                if !is_ncname(&v) {
                    Some(format!("{value:?} is not a valid ID"))
                } else if !self.ids.insert(v.clone()) {
                    Some(format!("ID {v:?} is not unique"))
                } else {
                    None
                }
            }
            Simple::Enum { base, values } => {
                let v = if matches!(**base, Simple::String) { value.to_owned() } else { collapse(value) };
                if let Some(p) = self.simple_problem(base, value) {
                    return Some(p);
                }
                (!values.contains(&v)).then(|| format!("{value:?} is not one of [{}]", values.join(", ")))
            }
        }
    }

    fn element(&mut self, node: NodeId, ty: TypeId) {
        let schema = self.schema;
        let def = &schema.types[ty];
        let doc = self.doc;
        let tree = &doc.tree;
        let n = tree.node(node);
        let tag = n.tag.clone();
        if !matches!(def.content, Content::Any) {
            for (k, value) in &n.attrs {
                // Namespace declarations and schema-location hints are inert; other
                // instance attributes (xsi:type, xsi:nil) change validity in ways this
                // validator does not implement.
                if k.starts_with("xmlns") || k == "xsi:noNamespaceSchemaLocation" || k == "xsi:schemaLocation" {
                    continue;
                }
                if k.starts_with("xsi:") {
                    self.error(node, &format!("<{tag}> attribute {k} is not supported by this validator"));
                    continue;
                }
                match def.attrs.iter().find(|a| a.name == *k) {
                    None => self.error(node, &format!("<{tag}> attribute {k} is not allowed")),
                    Some(a) => {
                        if let Some(p) = self.simple_problem(&a.ty, value) {
                            self.error(node, &format!("<{tag}> attribute {k}: {p}"));
                        }
                    }
                }
            }
            for a in def.attrs.iter().filter(|a| a.required) {
                if !n.has_attr(&a.name) {
                    self.error(node, &format!("<{tag}> is missing the required attribute {}", a.name));
                }
            }
        }
        let children = n.children.clone();
        let text: String =
            n.items.iter().filter_map(|i| if let Item::Text(t) = i { Some(t.as_str()) } else { None }).collect();
        match &def.content {
            Content::Any => {}
            Content::Empty | Content::Simple(_) if !children.is_empty() => {
                self.error(node, &format!("<{tag}> must not contain elements (found <{}>)", tree.tag(children[0])));
            }
            Content::Empty => {
                if !def.mixed && !text.trim_matches([' ', '\t', '\r', '\n']).is_empty() {
                    self.error(node, &format!("<{tag}> must be empty"));
                }
            }
            Content::Simple(simple) => {
                if let Some(p) = self.simple_problem(simple, &text) {
                    self.error(node, &format!("<{tag}> text: {p}"));
                }
            }
            Content::Model(model) => {
                if !def.mixed && !text.trim_matches([' ', '\t', '\r', '\n']).is_empty() {
                    self.error(node, &format!("<{tag}> does not allow text here"));
                }
                let tags: Vec<&str> = children.iter().map(|&c| tree.tag(c)).collect();
                let mut furthest = 0;
                let ends = match_particle(model, &tags, 0, &Assign::default(), &mut furthest);
                if let Some(assign) = ends.get(&tags.len()) {
                    for (&child, cty) in children.iter().zip(assign.to_vec()) {
                        self.element(child, cty);
                    }
                } else {
                    let message = if furthest < tags.len() {
                        format!("<{}> is not expected here inside <{tag}>", tags[furthest])
                    } else {
                        format!("<{tag}> is missing required child elements")
                    };
                    self.error(children.get(furthest).copied().unwrap_or(node), &message);
                    // Keep validating the children whose declaration is unambiguous by name.
                    for &child in &children {
                        if let Some(cty) = decl_by_name(model, tree.tag(child)) {
                            self.element(child, cty);
                        }
                    }
                }
            }
        }
    }
}

fn decl_by_name(p: &Particle, name: &str) -> Option<TypeId> {
    match p {
        Particle::Element { name: n, ty, .. } => (n == name).then_some(*ty),
        Particle::Sequence { items, .. } | Particle::Choice { items, .. } => {
            items.iter().find_map(|i| decl_by_name(i, name))
        }
    }
}

type Ends = BTreeMap<usize, Assign>;

fn bounds(p: &Particle) -> (usize, Occurs) {
    match p {
        Particle::Element { min, max, .. }
        | Particle::Sequence { min, max, .. }
        | Particle::Choice { min, max, .. } => (*min, *max),
    }
}

/// End positions reachable by one occurrence of the particle's term, each with
/// the full assignment (`prefix` extended by this occurrence's children).
fn match_once(p: &Particle, tags: &[&str], pos: usize, prefix: &Assign, furthest: &mut usize) -> Ends {
    let mut out = Ends::new();
    match p {
        Particle::Element { name, ty, .. } => {
            if tags.get(pos) == Some(&name.as_str()) {
                out.insert(pos + 1, prefix.push(*ty));
                *furthest = (*furthest).max(pos + 1);
            }
        }
        Particle::Sequence { items, .. } => {
            let mut frontier = Ends::from([(pos, prefix.clone())]);
            for item in items {
                let mut next = Ends::new();
                for (start, assign) in &frontier {
                    for (end, full) in match_particle(item, tags, *start, assign, furthest) {
                        next.entry(end).or_insert(full);
                    }
                }
                frontier = next;
                if frontier.is_empty() {
                    break;
                }
            }
            out = frontier;
        }
        Particle::Choice { items, .. } => {
            for item in items {
                for (end, full) in match_particle(item, tags, pos, prefix, furthest) {
                    out.entry(end).or_insert(full);
                }
            }
        }
    }
    out
}

/// End positions reachable by the particle including its occurrence bounds.
fn match_particle(p: &Particle, tags: &[&str], pos: usize, prefix: &Assign, furthest: &mut usize) -> Ends {
    let (min, max) = bounds(p);
    let cap = match max {
        Occurs::Bounded(m) => m,
        // Past `min`, only occurrences that consume an element can help.
        Occurs::Unbounded => min + (tags.len() - pos) + 1,
    };
    let mut results = Ends::new();
    if min == 0 {
        results.insert(pos, prefix.clone());
    }
    let mut frontier = Ends::from([(pos, prefix.clone())]);
    for k in 1..=cap {
        let mut next = Ends::new();
        for (start, assign) in &frontier {
            for (end, full) in match_once(p, tags, *start, assign, furthest) {
                if end == *start && k > min {
                    continue;
                }
                next.entry(end).or_insert(full);
            }
        }
        if next.is_empty() {
            break;
        }
        if k >= min {
            for (end, assign) in &next {
                results.entry(*end).or_insert_with(|| assign.clone());
            }
        }
        frontier = next;
    }
    results
}

/// The schema directory: explicit, then `DART_SCHEMA_DIR`, then DART Editor's default.
pub fn schema_dir(explicit: Option<&Path>) -> Result<Option<PathBuf>> {
    if let Some(dir) = explicit {
        if !dir.is_dir() {
            return Err(Error::new(
                crate::ErrorCode::FileNotFound,
                format!("{}: schema directory not found", dir.display()),
            ));
        }
        return Ok(Some(dir.to_path_buf()));
    }
    let env = std::env::var_os("DART_SCHEMA_DIR").filter(|v| !v.is_empty()).map(PathBuf::from);
    Ok(env.into_iter().chain([PathBuf::from(DEFAULT_SCHEMA_DIR)]).find(|p| p.is_dir()))
}

/// Schema validation issues for `check`.
pub fn check(doc: &Doc, explicit_dir: Option<&Path>) -> Result<Vec<Issue>> {
    let dir = schema_dir(explicit_dir)?;
    let schema = header_info(doc).schema;
    if schema.is_empty() {
        return Ok(vec![Issue::info("schema-skipped", "contents.xml declares no schema; schema validation skipped")]);
    }
    let Some(dir) = dir else {
        return Ok(vec![Issue::info(
            "schema-skipped",
            "no DART schema directory found (pass --schema-dir or set DART_SCHEMA_DIR); schema validation skipped",
        )]);
    };
    let entries = std::fs::read_dir(&dir).map_err(|e| Error::io(&dir, &e))?;
    let found = entries
        .filter_map(std::result::Result::ok)
        .map(|e| e.path())
        .find(|p| p.file_name().is_some_and(|n| n.to_string_lossy().eq_ignore_ascii_case(&schema)));
    let Some(path) = found else {
        return Ok(vec![Issue::warning("schema-missing", format!("{schema} not found in {}", dir.display()))]);
    };
    let file = path.file_name().map(|n| n.to_string_lossy().into_owned()).unwrap_or_default();
    let data = std::fs::read(&path).map_err(|e| Error::io(&path, &e))?;
    let compiled = match Schema::compile(&data) {
        Ok(s) => s,
        Err(e) => {
            return Ok(vec![Issue::warning(
                "schema-unsupported",
                format!("{file}: {}; schema validation skipped", e.message),
            )]);
        }
    };
    let errors = compiled.validate(doc);
    if errors.is_empty() {
        return Ok(vec![Issue::info("schema-valid", format!("contents.xml validates against {file}"))]);
    }
    let mut out: Vec<Issue> =
        errors.iter().take(MAX_REPORTED).map(|m| Issue::error("schema-invalid", m.clone())).collect();
    if errors.len() > MAX_REPORTED {
        out.push(Issue::error("schema-invalid", format!("... {} more schema errors", errors.len() - MAX_REPORTED)));
    }
    Ok(out)
}

#[cfg(test)]
mod tests {
    use super::*;

    const XSD: &str = r#"<?xml version="1.0"?>
<xs:schema xmlns:xs="http://www.w3.org/2001/XMLSchema" elementFormDefault="qualified">
  <xs:element name="ROOT">
    <xs:complexType>
      <xs:sequence>
        <xs:element name="HEAD" type="headType"/>
        <xs:choice minOccurs="0" maxOccurs="unbounded">
          <xs:element name="P" type="pType"/>
          <xs:group ref="extras"/>
        </xs:choice>
      </xs:sequence>
    </xs:complexType>
  </xs:element>
  <xs:group name="extras"><xs:sequence><xs:element name="BR" type="emptyType"/><xs:element name="BOX" type="boxType" minOccurs="0"/></xs:sequence></xs:group>
  <xs:complexType name="boxType"><xs:sequence><xs:group ref="extras" minOccurs="0"/></xs:sequence></xs:complexType>
  <xs:complexType name="emptyType"><xs:attribute name="ID" type="xs:ID"/></xs:complexType>
  <xs:complexType name="headType">
    <xs:simpleContent><xs:extension base="xs:string">
      <xs:attribute name="CODE" type="xs:string" use="required"/>
    </xs:extension></xs:simpleContent>
  </xs:complexType>
  <xs:complexType name="pType" mixed="true">
    <xs:choice minOccurs="0" maxOccurs="unbounded"><xs:element name="P" type="pType"/></xs:choice>
    <xs:attribute name="ALIGN" type="alignType"/>
  </xs:complexType>
  <xs:simpleType name="alignType">
    <xs:restriction base="xs:NMTOKEN"><xs:enumeration value="LEFT"/><xs:enumeration value="RIGHT"/></xs:restriction>
  </xs:simpleType>
</xs:schema>"#;

    fn errors(doc: &str) -> Vec<String> {
        let schema = Schema::compile(XSD.as_bytes()).unwrap();
        schema.validate(&Doc::parse(doc.as_bytes().to_vec()).unwrap())
    }

    #[test]
    fn accepts_a_valid_document() {
        assert_eq!(
            errors(
                r#"<ROOT><HEAD CODE="1">t</HEAD><P ALIGN=" LEFT ">a<P>b</P></P><BR ID="x"/><BOX><BR/></BOX><P/></ROOT>"#
            ),
            Vec::<String>::new()
        );
    }

    #[test]
    fn reports_content_attribute_and_value_errors() {
        let e = errors("<ROOT>\n<P/><HEAD/></ROOT>");
        assert!(e[0].contains("line 2: <P> is not expected here inside <ROOT>"), "{e:?}");
        let e = errors(r#"<ROOT><HEAD CODE="1" X="2"/><P ALIGN="UP"/><BR ID="1a"/><BR ID="b"/><BR ID="b"/></ROOT>"#);
        assert_eq!(e.len(), 4, "{e:?}");
        assert!(errors("<ROOT><HEAD/></ROOT>")[0].contains("required attribute CODE"));
        assert!(errors(r#"<ROOT><HEAD CODE="1"><P/></HEAD></ROOT>"#)[0].contains("must not contain elements"));
        assert!(errors(r#"<ROOT><HEAD CODE="1"/>text</ROOT>"#)[0].contains("does not allow text"));
    }

    #[test]
    fn instance_attributes_other_than_location_hints_are_errors() {
        assert!(
            errors(r#"<ROOT xmlns:xsi="x" xsi:noNamespaceSchemaLocation="a.xsd"><HEAD CODE="1"/></ROOT>"#).is_empty()
        );
        let e = errors(r#"<ROOT xmlns:xsi="x"><HEAD CODE="1" xsi:nil="true"/></ROOT>"#);
        assert!(e[0].contains("xsi:nil is not supported"), "{e:?}");
    }

    #[test]
    fn long_assignment_chains_drop_without_recursion() {
        let mut chain = Assign::default();
        for i in 0..1_000_000 {
            chain = chain.push(i);
        }
        drop(chain);
    }

    #[test]
    fn refuses_unsupported_constructs() {
        let xsd = XSD.replace("<xs:group ref=\"extras\"/>", "<xs:any/>");
        assert!(Schema::compile(xsd.as_bytes()).unwrap_err().message.contains("unsupported"));
    }
}
