# Foot a document

Footing confirms that totals, subtotals, movements, and cross-references
add up. The CLI computes every figure; you only judge which cells should
relate. Never add, subtract, or compare amounts yourself, and do not change
figures unless the user asks (then follow [editing](editing.md)).

`foot` is newer than the other commands: check `dsd foot --help` before
relying on an option described here.

## The model

Every check is `target = Σ sign × term`, over `T/R/C` cells:

```json
{"id": "v:T3.R6.C2", "kind": "vertical", "origin": "auto", "rule": "label-total",
 "target": {"table": 3, "row": 6, "col": 2, "rowLabel": "자 산 총 계", "colLabel": "제 5 기", "section": "재무상태표"},
 "terms": [{"sign": 1, "table": 3, "row": 3, "col": 2}]}
```

A check passes only when its difference (target − Σ terms) is 0, unless it
states a tolerance. A blank cell or a lone `-` counts as 0. Tables in
different currency units are converted. A text cell, an unknown unit, or a
cell outside its table makes the check `needsJudgment`, with a `reason`.

## Workflow

1. **Run** `dsd foot FILE`. Read `result.counts`, then these lists:
   - `checks`: proposed checks, each `pass`, `fail`, or `needsJudgment`;
   - `unresolved`: totals the labels imply that no structure explains,
     each with its closest candidate already evaluated;
   - `candidates`: statement amounts found in a named note for one period
     only;
   - `uncovered`: totals with nothing to sum, and statement amounts their
     notes do not show.

   The `rule` of each check says which proposer stated it (`dsd foot --help`
   lists them).
2. **Review** what is not a clean pass, in this order: `fail`, `unresolved`,
   `needsJudgment`, `candidates`, `uncovered`. Inspect the table with
   `dsd table FILE N --format text`, looking at the target row and the term
   ranges. For each one, decide:
   - **A true difference:** the structure is right and the figures disagree.
     Keep it for the report.
   - **The wrong structure:** for example a subtotal summed with its own
     parts, a rate or count column, or a sign. State the right relation as
     a replacement.
   - **Not a relation:** for example a 계 that is a label, or a coincidence.
     Dismiss it with a reason.
   - **A missing relation:** an uncovered total, or a cross-reference the
     CLI did not see. Add a check.
3. **Write a checks file** holding only your decisions:

   ```json
   {"checks": [
     {"id": "v:T12.R9.C2", "kind": "vertical", "origin": "agent",
      "target": {"table": 12, "row": 9, "col": 2, "rowLabel": "합 계", "colLabel": "당기말", "section": "7. 기타자산"},
      "terms": [{"sign": 1, "table": 12, "row": 5, "col": 2}, {"sign": 1, "table": 12, "row": 8, "col": 2}],
      "note": "합계 is the 소계 above plus the one item below it"}],
    "dismiss": [
     {"id": "v:T30.R4.C2", "target": {"table": 30, "row": 4, "col": 2, "rowLabel": "부채총계", "colLabel": "당기말", "section": "30. 자본위험관리"},
      "reason": "a ratio table: 부채총계 is an input to the ratio, not the sum of the rows above"}]}
   ```

   - **Targets:** copy them from the result, so the labels and section are
     the ones the document has. For an `uncovered` entry, drop its `kind`.
   - **Replace or add:** a check with a proposal's id and target replaces
     that proposal; give an added check a new id, such as `a:` plus a short
     name.
   - **Notes and reasons:** every check needs a `note` saying why the
     relation holds, and every dismissal a `reason`.
   - **Tolerance:** state one only for a stated reason, for example a
     rounded table in 천원 (`"tolerance": {"perTerm": 1}`), and say so in
     the report.
4. **Rerun** with `dsd foot FILE --checks checks.json`. An entry whose labels
   are no longer at its cell does not apply:
   - `moved`: the labels are at one other cell, given in `found`. Confirm
     it in the table, then update the entry's target and terms. A
     replacement or dismissal must also take the current proposal's id
     from the new result (`v:T12.R10.C2`), because a proposal's id names
     its cell; an added check keeps its own id.
   - `stale`: the labels are gone or ambiguous. Re-inspect, then restate or
     remove the entry.

   Repeat until every `fail` is a true difference you can explain.
5. **Write the workbook** when the user wants to look or correct:
   `dsd foot FILE --checks checks.json -o footing.xlsx`.
   - **In the tables:** each check is a live difference formula, below its
     table (checks down a column) or beside it (checks across a row). It
     shows ✓ at 0 and red beyond its allowance.
   - **`검증` sheet:** lists every check, including those between tables.
   - **`총괄표`:** counts each sheet's checks by formula, so the counts
     follow corrections typed into the workbook.

   `result.workbook.tables` gives each table's range in this workbook. The
   ranges differ from `dsd xlsx`, because check rows push content down.

## Report

- **True differences:** each with its `T#R#C#`, row and column labels,
  section, the actual and expected amounts, the difference, and the unit.
- **Kept checks:** tolerances you stated, and replacements whose structure
  differs from the proposal, each with its note.
- **Removed checks:** dismissals with their reasons.
- **Open items:** what is still `needsJudgment`, `unresolved`, a candidate,
  or uncovered, and why.
- **Counts:** checks passed and failed.

Keep the checks file with the document; it carries the document's labels,
so it is as confidential as the document. On the next draft, rerun with the
same file, and resolve what moved or went stale.
