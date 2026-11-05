//! Help text that cannot live in clap attributes directly.

/// `apply` field reference; a test keeps it equal to the library's op table.
pub const APPLY_AFTER: &str = "\
Edit objects (required fields first; ? marks optional ones):
  set-cell   table: int, row: int, col: int, text: str, unit_value?: str, flatten?: bool
  replace    old: str, new: str, expect?: int|\"any\", table?: int
  copy-row   table: int, row: int, count?: int, clear?: bool, force_locked?: bool
  delete-row table: int, row: int, force_locked?: bool

Edits run in order and later ones see earlier results: row numbers refer to the
table after earlier insertions or deletions, so list them bottom-up or account
for the shift. Nothing is written unless every edit succeeds.

Example:
  [{\"op\": \"set-cell\", \"table\": 3, \"row\": 4, \"col\": 2, \"text\": \"1,100,000\"},
   {\"op\": \"replace\", \"old\": \"2024년\", \"new\": \"2025년\", \"expect\": 2}]";

#[cfg(test)]
mod tests {
    #[test]
    fn apply_help_lists_the_library_ops() {
        for line in dsd::edit::op_reference() {
            assert!(super::APPLY_AFTER.contains(&format!("  {line}\n")), "{line}");
        }
    }
}
