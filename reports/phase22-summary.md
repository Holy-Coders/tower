# Phase 2.2 summary — the three deep-playtest fixes

All three findings from the deep playtest of Phase 2.1 (N1–N3, as
`briefs/PHASE-2.2-BRIEF.md` Fixes A–D) are implemented, tested, and
demoed. Changes are uncommitted in the working tree (no commits, no
pushes, per the brief). Verified: `cargo build --release` warning-free,
`cargo test` **37/37**, `scripts/demo.sh` **51/51**,
`scripts/demo-phase21.sh` **47/47**, `scripts/demo-phase22.sh` **60/60**
(all exit 0). Design rationale is in `DECISIONS.md` entries 21–24.

## What changed

- **Fix A (N1) — landing no longer releases claims**
  (`src/engine.rs: finalize_landing`): the silent `release_claims` call
  is gone. Claims belong to the assignment and are released only by
  explicit `claim release` or by `assignment close` (which already
  released them; now pinned by test). The `land` JSON receipt and the
  `changeset_landed` event payload both carry `claims_held`
  (`[{"file", "item"}]`; the event also gains `assignment_id`), so claim
  state after a landing is explicit. A continuing assignment's next
  changeset no longer flags earlier-round claimed territory as
  outside-claim, and the board's CLAIMS section stays populated while
  assignments are open. The now-dead `Store::release_claims` wrapper
  was removed (owner's no-shims ruling).
- **Fix B (N2) — Rust test functions indexed individually**
  (`src/index.rs: walk_rust`): inline modules are walked as containers,
  so each function inside `#[cfg(test)] mod tests` is its own item named
  by the function — the synthetic `tests` item is gone, with no
  name-/attribute-based special case anywhere (grep-verified). A claim
  on one test function does not cover edits to another; a whole-file
  claim covers all. TypeScript was checked and has no analogous
  synthetic item; it is unchanged (documented in DECISIONS 22 and the
  README limitations).
- **Fix C (N2) — outside-claim detail surfaced with the hint**
  (`src/engine.rs: claimed_vs_actual / outside_reason`,
  `src/board.rs: changeset_status_value / render_text`): changed items
  flow as `(file, item)` pairs, making coverage file-scoped (fixing two
  flattening bugs: any whole-file claim covered every file's items, and
  same-named item claims cross-covered between files). `outside_items`
  entries are now `{"file", "item"}` objects like `outside_files`
  (deliberate breaking shape change, owner's ruling); a wholly outside
  file is reported once instead of once per item. Reasons now read
  "touched files outside claim: src/orders.rs#discount_is_tenth,
  src/notify.rs (whole file)" on the text board, in `changeset status`,
  and over MCP. CLI and MCP `changeset_status` share one builder that
  returns all previous fields plus `claimed_vs_actual` in the
  `board --json` shape.
- **Fix D (N3 light) — conflict detail without leaving Tower**
  (`src/engine.rs: changeset_conflicts`, `src/git.rs:
  merge_tree_conflict_hunks`, CLI `changeset conflicts [--json]`, MCP
  `changeset_conflicts`, tools 18 → 19): read-only; for each file the
  changeset conflicts in against main and/or earlier unlanded
  changesets (matching its red reasons), it prints the file, the
  counterparty, and the `@@` hunk headers parsed from classic
  three-argument `git merge-tree` "changed in both" output (the
  `--write-tree` form names files but has no hunks; both probed on git
  2.43). Non-conflicted, landed, and superseded changesets report an
  empty conflict list with exit 0. No resolution, no worktree writes.

Intentional behavior changes, each called out (owner's ruling — no
shims added): claims now survive landing (Fix A); inline Rust module
contents are indexed instead of a module item (Fix B, generalized past
`#[cfg(test)]`); `outside_items` changed from bare strings to
`{"file", "item"}` objects and claim coverage became file-scoped
(Fix C); MCP surface 18 → 19 tools (Fix D); `Store::release_claims`
removed. Existing-test/demo updates were kept to exactly what those
require: the `claimed_vs_actual` unit test was rewritten to the new
pair/object semantics, and the MCP tool count was updated in
`src/tests.rs` and `scripts/demo.sh` (still 51 assertions). No old
test or demo asserted release-on-land, so Fix A needed no rewording
there. No DB schema change was needed.

## Tests / demos

- `cargo test`: 37 tests (7 new Phase 2.2 tests, written first and
  observed failing on Phase 2.1 code before each fix):
  `landing_does_not_release_claims_and_reports_claims_held` (incl.
  continuation re-register staying inside-claim),
  `assignment_close_releases_all_of_the_assignments_claims`,
  `rust_indexer_emits_individual_test_functions_not_a_tests_item`,
  `claim_on_one_test_function_does_not_cover_another`,
  `changeset_status_carries_claimed_vs_actual_and_board_names_outside_items`
  (CLI/MCP payload, board CVA identity, text-board reason line),
  `changeset_conflicts_names_file_and_hunk_header` (vs earlier
  changeset, empty cases, read-only proof, unknown ID),
  `changeset_conflicts_against_main_after_main_advances`.
- `scripts/demo.sh`: 51/51 (one assertion reworded: MCP tool count).
- `scripts/demo-phase21.sh`: 47/47, unchanged.
- `scripts/demo-phase22.sh` (new, 60/60): scratch Rust repo with a
  two-function `#[cfg(test)]` module. A-1 claims one test function,
  edits the other plus an unclaimed file — status/board/MCP name
  `src/orders.rs#discount_is_tenth` and `src/notify.rs (whole file)`,
  board JSON indexes both test functions and no `tests` item. Claims
  fixed, gates green, C-1 lands — all three claims still held
  (`claims_held` exact-match, claim list, landed event), released only
  by `assignment close`. Then a conflicting pair makes C-3 red and
  `changeset conflicts` (text, `--json`, MCP) names `src/orders.rs`
  vs C-2 with `@@` headers while main/worker heads stay untouched;
  C-2 reports no conflicts with exit 0.

## Left stubbed / deferred

Nothing in the Phase 2.2 scope is stubbed. Deliberately out of scope
and unchanged: conflict *resolution* (the conflicts command shows hunk
headers only, not full diffs; non-content conflicts name the file with
an empty hunk list), TypeScript test-item indexing (no synthetic item
existed to remove), and everything deferred before Phase 2.2 (polling
watch, creation-order integration, parsed-but-unenforced config,
`lsp|duplicates|github` stubs). Pre-2.2 stored rows need no migration:
claimed-vs-actual is recomputed live on refresh, and the text board
expands a stored plain reason from the stored CVA until then.

## Aaron's first commands

```sh
export PATH="$HOME/.cargo/bin:$PATH"
cd ~/workspace/code/holy-coders/tower
cargo build --release
cargo test                              # 37/37
bash scripts/demo-phase22.sh            # 60/60: watch claims survive a landing

# Against any registered repo with a red changeset, e.g. C-3:
./target/release/tower --repo <repo> changeset status C-3          # now carries claimed_vs_actual
./target/release/tower --repo <repo> changeset conflicts C-3       # files + @@ hunk headers
./target/release/tower --repo <repo> changeset conflicts C-3 --json
./target/release/tower --repo <repo> land C-1                      # receipt shows claims_held
./target/release/tower --repo <repo> claim list                    # claims still there after landing
```
