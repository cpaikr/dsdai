//! The verified-element lists agree with the skill's format reference and,
//! when installed, with the DART schemas.

use std::collections::{BTreeMap, BTreeSet};
use std::path::{Path, PathBuf};

use dsd::doc::{OBSERVED_SCHEMAS, OBSERVED_TAGS, SCHEMA_EQUIVALENT_TAGS};
use dsd::xml::{self, NodeId, Tree};

fn repo() -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR")).join("../..")
}

fn backticked(text: &str) -> BTreeSet<String> {
    text.split('`')
        .skip(1)
        .step_by(2)
        .filter(|t| t.chars().next().is_some_and(|c| c.is_ascii_alphabetic()))
        .filter(|t| t.chars().all(|c| c.is_ascii_alphanumeric() || c == '-'))
        .map(str::to_owned)
        .collect()
}

#[test]
fn verified_tags_match_the_format_reference() {
    let text = std::fs::read_to_string(repo().join("skills/dart-dsd/references/format.md")).unwrap();
    let coverage = text.split("## Verified coverage").nth(1).unwrap();
    let (observed, rest) = coverage.split_once("Verified by schema equivalence").unwrap();
    let equivalent = rest.split("\n\n").nth(1).unwrap();
    let set = |tags: &[&str]| tags.iter().map(|t| (*t).to_owned()).collect::<BTreeSet<_>>();
    assert_eq!(backticked(observed), set(&OBSERVED_TAGS));
    assert_eq!(backticked(equivalent), set(&SCHEMA_EQUIVALENT_TAGS));
}

#[test]
fn the_public_fixture_uses_only_observed_tags() {
    let xml = std::fs::read(repo().join("fixtures/public/sample/contents.xml")).unwrap();
    let tree = xml::parse(&xml).unwrap();
    for n in tree.descendants(tree.root) {
        assert!(OBSERVED_TAGS.contains(&tree.tag(n)), "{}", tree.tag(n));
    }
}

/// Structure and attributes of a schema element, ignoring annotations, the
/// type's own name, and nested section levels (their number, or absence at
/// the deepest level).
fn shape(tree: &Tree, node: NodeId, top: bool) -> Option<String> {
    let n = tree.node(node);
    if n.tag.ends_with(":annotation") {
        return None;
    }
    let ident = n.attr("name").or_else(|| n.attr("ref")).unwrap_or("");
    let lower = ident.to_ascii_lowercase();
    let is_section =
        lower.strip_prefix("section-").is_some_and(|d| !d.is_empty() && d.bytes().all(|b| b.is_ascii_digit()));
    if !top && n.tag.ends_with(":element") && is_section {
        return None;
    }
    let attrs: BTreeMap<&str, &str> =
        n.attrs.iter().filter(|(k, _)| !(top && k == "name")).map(|(k, v)| (k.as_str(), v.as_str())).collect();
    let kids: Vec<String> = n.children.iter().filter_map(|&c| shape(tree, c, false)).collect();
    Some(format!("{}{attrs:?}[{}]", n.tag, kids.join(",")))
}

#[test]
fn schema_equivalent_tags_match_installed_schemas() {
    // Reads the locally installed (private, uncommitted) DART schemas at run
    // time; no schema text or derived values live in the repository.
    let dir = repo().join("fixtures/private/dart-resource");
    let Ok(entries) = std::fs::read_dir(&dir) else {
        eprintln!("skipped: installed DART schemas absent (fixtures/private/)");
        return;
    };
    let pairs = [
        ("SECTION-4", "SECTION-2"),
        ("SECTION-5", "SECTION-2"),
        ("SECTION-6", "SECTION-2"),
        ("SECTION-7", "SECTION-2"),
        ("TFOOT", "TBODY"),
    ];
    assert_eq!(pairs.iter().map(|p| p.0).collect::<BTreeSet<_>>(), SCHEMA_EQUIVALENT_TAGS.iter().copied().collect());
    let mut checked = 0;
    // Only the schemas the equivalence claim is about; other files may sit beside them.
    let claimed = |p: &PathBuf| {
        p.file_name().is_some_and(|n| OBSERVED_SCHEMAS.contains(&n.to_string_lossy().to_lowercase().as_str()))
    };
    for path in entries.map(|e| e.unwrap().path()).filter(claimed) {
        let tree = xml::parse(&std::fs::read(&path).unwrap()).unwrap();
        let all = tree.descendants(tree.root);
        let types: BTreeMap<&str, NodeId> = all
            .iter()
            .filter(|&&n| tree.tag(n).ends_with(":complexType"))
            .filter_map(|&n| tree.node(n).attr("name").map(|name| (name, n)))
            .collect();
        let mut element_type: BTreeMap<&str, &str> = BTreeMap::new();
        for &n in &all {
            let node = tree.node(n);
            if node.tag.ends_with(":element")
                && let (Some(name), Some(ty)) = (node.attr("name"), node.attr("type"))
            {
                element_type.entry(name).or_insert(ty);
            }
        }
        for (tag, base) in pairs {
            let a = shape(&tree, types[element_type[tag]], true);
            let b = shape(&tree, types[element_type[base]], true);
            assert_eq!(a, b, "{tag} vs {base} in {}", path.file_name().unwrap().to_string_lossy());
            checked += 1;
        }
    }
    assert!(checked > 0);
}
