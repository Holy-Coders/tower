# Phase 2.1 summary — the six playtest fixes

All six findings from `~/workspace/tower-playtest-long/REPORT.md` (F1–F6)
plus board next-actions are implemented, tested, and demoed. Changes are
uncommitted in the working tree. Verified: `cargo build --release`
warning-free, `cargo test` **30/30**, `scripts/demo.sh` **51/51**,
`scripts/demo-phase21.sh` **47/47** (all exit 0).

## What changed

- **F1 order-aware statuses** (`src/engine.rs: refresh_changeset_statuses`):
  red only for conflicts with main or earlier unlanded changesets; later
  conflicts are informational "blocks C-x in <files>" reasons. A
  conflicting set now lands sequentially in integration order with no
  pre-integration merges.
- **F5 landing candidates** (`land_changeset`, `build_landing_candidate`):
  green fast-forwards unchanged; otherwise Tower merges the committed
  head onto current main in its own `landing` scratch worktree, runs the
  configured gates there, re-checks both heads (rebuild once, then fail
  for retry), and advances main only to the candidate head
  (`--ff-only` in a clean checked-out main worktree, else CAS
  `update-ref`). Yellow is landable only when its blockers are all
  pending/stale gate proofs. `changeset_landed` events now carry
  `source_head`, `candidate_head`, `based_on_main_head`, and `method`.
- **F6 gate proofs** (`gate_runs.tree_hash/main_head/participants_json`):
  a pass counts only while main and every participant snapshot are
  unchanged; otherwise yellow "gate proof stale (source, peer, or main
  changed) — rerun gates". Pre-2.1 proof-less rows are grandfathered.
- **F2+F4 refresh/supersede**: claimed-vs-actual is recomputed live on
  every status refresh; `tower changeset refresh <id>` (MCP
  `changeset_refresh`) bumps a new `version` column and records the head.
  `superseded` state (flag + status + `superseded_by`) is set by
  re-registering an assignment, by landing a task sibling, or manually
  via `tower changeset supersede` / `changeset_supersede`. Superseded
  records are out of the integration order, status refresh, radar
  pairing, and gate attribution, and appear in a short board SUPERSEDED
  section instead of the main list.
- **F3 claims**: `claim acquire` grants the uncontested subset; response
  JSON keeps `granted` (all-or-nothing) and adds `granted_nodes` and
  `refused` (file, item, holding assignment; `collisions` kept as an
  alias). CLI exit is 1 only when zero nodes were granted.
  `claim release --node file#item` (repeatable, MCP `nodes` array)
  releases exactly those nodes; no nodes = release all.
- **Board next-action** (`board::next_action`, pure): `land`, `run gates`,
  `rerun gates (proof stale)`, `resolve conflict with C-x in <files>`,
  `refresh claims or changeset`, `superseded`, `landed` — in text, JSON,
  and HTML boards and in `changeset status`.
- Additive migrations only (`src/store.rs`): a copy of the real v1
  playtest home opens and migrates in place (7 changesets); the original
  home was not modified. MCP surface is additive: 16 → 18 tools.

Intentional behavior changes recorded in `DECISIONS.md` (entries 14–20)
and reflected in tests/demo: first-of-conflicting-pair is no longer red,
gate-proof-only yellows can land via candidates, partially refused claim
bundles now grant the free subset, and the demo's refused-landing check
targets red C-2 (demo is still 51 assertions).

## Tests / demos

- `cargo test`: 30 tests (13 new) covering every bullet in the brief, plus
  CAS landing with main not checked out, candidate gate failure,
  superseded radar/gate exclusion, and a v1-schema migration test.
- `scripts/demo.sh`: 51/51, with the three intentional-change steps
  reworded (order-aware C-1, red C-2 refusal, 18 MCP tools).
- `scripts/demo-phase21.sh` (new, 47/47): C-1/C-2 fork from one base with
  compatible changes, C-2 stops fast-forwarding after C-1 lands; both
  land sequentially via candidates with worker worktrees/HEADs
  byte-untouched; conflicting C-3 is refused naming `src/greet.ts` with
  main unchanged. Also exercises partial claims (exit 0/1), per-node
  release, and next-actions.

## Left stubbed / deferred

Nothing in the Phase 2.1 scope is stubbed. Candidates never attempt
conflict resolution (refuse-with-files by design). The moved-head
rebuild-once path is race logic without a concurrency test; the
expected-head checks and CAS advance that implement it are covered by
the checked-out and not-checked-out landing tests. Pre-2.1 deferred
items from `PROGRESS.md` (polling watch, creation integration order,
parsed-but-unenforced config sections, `lsp|duplicates|github` stubs)
are unchanged.

## Aaron's first commands

```sh
export PATH="$HOME/.cargo/bin:$PATH"
cd ~/workspace/code/holy-coders/tower
cargo build --release
cargo test                              # 30/30
bash scripts/demo-phase21.sh            # 47/47: watch C-1/C-2 land via candidates

# Against the playtest fixture (uses its existing v1 home; migrates in place):
export TOWER_HOME=~/workspace/tower-playtest-long/home
./target/release/tower --repo ~/workspace/tower-playtest-long/repo board
./target/release/tower --repo ~/workspace/tower-playtest-long/repo changeset supersede C-1
./target/release/tower --repo ~/workspace/tower-playtest-long/repo changeset status C-2
```
