# Tower Phase 2.2 — Brief

Source: deep playtest of Phase 2.1 (2026-10-25, 4 tasks × 4 rounds, 6 changesets;
report at `~/workspace/tower-playtest-deep/REPORT.md`, full trail in its `evidence.log`).
Phase 2.1 itself is verified working. This phase fixes the three findings that run exposed.
Work in this repo (`~/workspace/code/holy-coders/tower`). **Make no commits and no pushes.**
Leave all changes in the working tree.

**Owner's ruling (2026-10-25): Tower is not in use anywhere yet, so breaking changes are
allowed and preferred when they make the design cleaner.** Do not preserve backward
compatibility for its own sake: stored data and existing Tower homes (including playtest
homes) need not survive, DB schema changes may be destructive rather than additive-only,
and CLI/MCP output shapes may change where the new shape is better. Where this brief below
asks for a compatibility shim, implement the clean version instead and say so in the summary.
(A previous run of this brief was stopped during its exploration phase, before source
edits; check `git status` and disregard any stray partial edits you find.)

## Fix A — Landing must not silently release the assignment's claims (playtest N1)

Observed: after C-1 landed, every claim held by A-1 was gone from `claim list`; same for A-2
after C-2. A continuing assignment (C-5/C-6 were continuations of A-1/A-2) got no warning —
its new changeset initially flagged items it had legitimately claimed in earlier rounds as
outside-claim, because the claims no longer existed. The final board's CLAIMS section was
empty even though two assignments were still open and working.

Required behavior:
- Landing a changeset **does not release any claims**. Claims belong to the assignment, not
  the changeset.
- Claims are released only by (a) explicit `claim release`, or (b) `assignment close`.
  Verify `assignment close` releases all of the assignment's claims; implement it if missing.
- The `land` command's JSON output gains a `claims_held` field: the assignment's claims
  still held after the landing (file + item), so claim state after a landing is explicit in
  the landing receipt itself. Record the same list in the landed event payload.
- Update any existing tests/demos that assert the old release-on-land behavior to the new
  semantics, and note the semantic change in DECISIONS.md.

## Fix B — Index individual Rust test functions (playtest N2, indexer half)

Observed: the Rust indexer collapses each file's whole `#[cfg(test)] mod tests` into a single
item named `tests`. Any test edit therefore counts as touching item `tests`, and C-1/C-2/C-4
all sat yellow until their assignees claimed `file#tests`. Item-level claims can never cover
a normal change that includes tests for one specific function.

Required behavior:
- The Rust indexer emits each test function inside a `#[cfg(test)]` module as its own item,
  named by the function name, exactly like non-test functions.
- Remove the synthetic `tests` item entirely. There is no `file#tests` special case in
  claimed-vs-actual matching: a touched test function is inside-claim when the assignee
  holds that function's item claim (or a whole-file claim on the file, which already
  exists as a claim form). Cleanly remove any code that special-cases the name `tests`.
- Scope: Rust indexer only. If the TypeScript indexer has an analogous synthetic item, leave
  it and note that in DECISIONS.md.

## Fix C — Surface outside-claim detail where the hint is printed (playtest N2, output half)

Observed: the text board says only "touched files outside claim"; the actual outside items
appear only in `board --json` (`outside_items`). `changeset status` JSON omits
claimed-vs-actual entirely, so the command that prints `next: refresh claims or changeset`
cannot tell the user what to claim.

Required behavior:
- `changeset status` JSON gains `claimed_vs_actual` with the same shape as `board --json`.
- Text board changeset lines name the outside items/files on the reason line, e.g.
  `touched files outside claim: src/orders.rs#tests, src/notify.rs (whole file)`.
- `changeset status` text output (if any) and the MCP changeset-status tool surface the same
  detail (MCP: include `claimed_vs_actual` in the tool result). Keep all existing fields.

## Fix D — Conflict detail command (playtest N3, light version)

Observed: resolving C-5's conflict was the most labor-intensive step of the run. The status
names the conflicted file, but the user must leave Tower and run raw git to see the hunks.

Required behavior:
- New CLI command `tower changeset conflicts <ID>`: read-only; for each file the changeset
  conflicts in (against main and/or earlier unlanded changesets, matching the status's red
  reasons), print the file and the conflicting hunk headers from `git merge-tree` output
  (the `@@` lines / changed-in-both sections). No resolution, no worktree writes.
- Expose it over MCP as a `changeset_conflicts` tool (MCP surface 18 → 19 tools).
- For a non-conflicted changeset it prints/reports an empty conflict list, exit 0.

## Constraints

- No commits, no pushes.
- Per the owner's ruling above: breaking changes are fine. DB migrations may rewrite the
  schema outright; CLI/MCP fields may be renamed or restructured where the result is
  cleaner. Do not add compatibility shims, grandfathering, or dual-shape outputs.
- The regression bar is behavioral, not literal: `cargo build --release` warning-free,
  `cargo test` passing (30 tests at Phase 2.1's end, plus new ones), `scripts/demo.sh`
  and `scripts/demo-phase21.sh` passing end to end. Where a fix changes semantics an old
  test or demo asserts (Fix A and Fix B both do), update that test/demo to the new
  semantics — as many assertions as needed — and call each change out in the summary.

## Deliverables

- New unit/integration tests for each fix, including: land-then-claims-still-held;
  assignment-close releases claims; indexer emits individual test-function items and a claim
  on one test function does not cover edits to another; `changeset status` JSON containing
  `claimed_vs_actual` with the right `outside_items`; `changeset conflicts` naming the file
  and a hunk header on a genuinely conflicting changeset.
- New `scripts/demo-phase22.sh` in the style of the previous demos (scratch repo, PASS/FAIL
  per assertion + summary) exercising A–D end to end.
- Update `README.md`, append Phase 2.2 entries to `DECISIONS.md` (next free numbers after 20),
  write `PROGRESS-2.2.md` and `reports/phase22-summary.md` (what changed per fix, test/demo
  counts, anything deferred and why).
