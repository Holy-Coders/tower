# Tower — Progress

## Status: COMPLETE (continuation run 1, 2026-10-25)

All acceptance items from `briefs/BUILD-BRIEF.md` pass. Final numbers:
`cargo build --release` clean (no warnings), `cargo test` **17/17 green**,
`scripts/demo.sh` **51/51 assertions PASS**, exit 0.

## What the interrupted run had left

All source modules existed and compiled (`src/config.rs`, `src/store.rs`,
`src/git.rs`, `src/index.rs`, `src/engine.rs`, `src/board.rs`, `src/mcp.rs`,
`src/ops.rs`, `src/main.rs`), covering the full Phase 1 + 2 surface — but
nothing was verified: 0 tests, no demo, no README.

## Stage log (this run)

1. **Assess** — `cargo build` OK (3 dead-code warnings). Read all sources;
   wrote this file.
2. **Smoke test** (`/tmp/tower-smoke.sh`) — full flow worked first try:
   init → radar red pair → claims → MCP task/worktree/claim → register →
   gates tier 1+2 → green → land fast-forwarded main; red land refused.
3. **Bug fixes** (all covered by tests now):
   - Claim-less changesets were permanently yellow ("touched files outside
     claim"); now reported as *unclaimed* — outside only when claims exist.
   - Item-level outside touches (`outside_items`) did not affect status.
   - Joint rehearsal attributed gate verdicts to changesets that had
     conflicted out of the rehearsal tree.
   - **Rust tree-sitter grammar silently failed to load** (ABI 15 grammar vs
     `tree-sitter` 0.24's max 14) → all Rust files indexed empty. Fixed by
     pinning `tree-sitter = "0.25"` (resolved 0.25.10).
   - Tower's own rehearsal scratch worktree appeared in radar as a colliding
     peer worktree; radar now excludes it.
   - Minor: full per-tool JSON schemas in MCP `tools/list`; failed
     `worktree_create` no longer leaves a phantom open assignment; dead
     code removed (build is warning-free).
4. **Tests** (`src/tests.rs`, 17 tests) — sequential per-repo IDs + events,
   single-holder lease, escalation/decision roundtrip, **claims refusal
   naming the colliding assignment**, Rust+TS item extraction,
   changed-items diffing, claimed-vs-actual semantics, Tier-0 honesty,
   radar red pair / green unrelated, changeset status mirrors with named
   counterparties, **rehearsal leaves user branches/worktree files
   (including uncommitted edits) byte-identical**, outside-claim yellow,
   fast-forward-only-green land, gate-failure blocks land, Tower-managed
   worktree Tier-0 flow, MCP dispatch surface (16 tools), merge-sim
   primitive.
5. **Demo** (`scripts/demo.sh`, 51 assertions) — the 6-step acceptance demo,
   driving MCP by piping JSON-RPC into `tower serve-mcp`. Runs entirely in
   a mktemp scratch dir with a scratch `TOWER_HOME`; `KEEP=1` preserves it.
6. **Docs** — `README.md`, `DECISIONS.md` (13 entries), this file,
   `reports/final-summary.md`.

## Stubs / deferred (by design)

- `tower lsp|duplicates|github` print "not implemented in v1".
- Polling watch (2s) instead of a filesystem watcher.
- Integration order = changeset creation order (no dependency graph).
- Land is fast-forward only; landed work must be committed first.
- `[reserved]`, `[escalation] extra_triggers`, `[limits]` parsed, not
  enforced. Item indexing: Rust + TypeScript only; else file-level.
