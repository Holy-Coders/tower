# Tower — final summary (continuation run 1)

## Top finding

The interrupted run had actually written the entire Phase 1 + 2 source tree
and it compiled — but none of it was verified, and Rust code indexing was
silently broken by a tree-sitter grammar ABI mismatch. All of that is now
fixed, tested, and demoed: **release build clean, 17/17 tests green,
51/51 demo assertions PASS**.

## What was built

Tower, per `briefs/BUILD-BRIEF.md` (governing), `tower-spec.md` (what), and
`tower-build-prompt.md` (process). Rust, one binary `tower`, edition 2021.

- **Data layer** (`src/store.rs`): SQLite (rusqlite, bundled) under
  `~/.tower/` (`TOWER_HOME` overrides); sequential per-repo IDs
  (T-/A-/C-/E-/D-, never reused); append-only event log — every state
  change appends an event; single-orchestrator lease table.
- **Config** (`src/config.rs`): `tower.toml` via serde+toml; gate inference
  for Cargo/npm/Go on `init`; honest tier labelling (no gates → board says
  "Tier-0 simulation only").
- **Git adapter** (`src/git.rs`): shells out to system `git` only.
  Worktree discovery (`worktree list --porcelain`), merge simulation
  (`merge-tree --write-tree --name-only --merge-base=…`), and read-only
  snapshot commits (temp `GIT_INDEX_FILE`) so uncommitted work can be
  simulated without touching user index/branches/files.
- **Tree-sitter map** (`src/index.rs`): Rust + TypeScript top-level items
  (functions, structs/enums/traits/types, impls + methods; TS functions,
  classes + methods, interfaces, type aliases, arrow consts) with line
  ranges; per-item change detection vs merge-base; other languages fall
  back to file-level.
- **Engine** (`src/engine.rs`): pairwise + vs-main simulation with
  green/yellow/red and named reasons; claimed-vs-actual comparison;
  rehearsal on a Tower-owned scratch worktree updating
  `refs/tower/integration`; Tier-1/Tier-2 gates from `tower.toml` run on
  the rehearsal tree, attributed only to changesets that actually merged;
  `land` = fast-forward into main, refusing anything non-green.
- **Board + watch** (`src/board.rs`, `src/main.rs`): text/JSON/static-HTML
  board; 2s polling watch loop emitting collision and material-diff events
  (optional Tier-1 on change), rewriting the HTML board each scan.
- **Orchestrated mode** (`src/ops.rs`, `src/store.rs`): tasks with plans,
  assignments, item/file claims (grant, or the exact colliding assignment),
  Tower-managed worktrees under `~/.tower/worktrees/<repo-hash>/`, changeset
  registration, escalations/decisions, lease — mirrors in CLI and MCP.
- **MCP server** (`src/mcp.rs`): hand-rolled stdio JSON-RPC 2.0
  (`initialize`, `ping`, `tools/list`, `tools/call`), all 16 spec tools
  with full input schemas.
- **CLI** (`src/main.rs`): `init`, `board`, `watch`, `task`, `assignment`,
  `claim`, `worktree`, `changeset`, `gate`, `merge-check`, `order`,
  `escalate`, `decisions`, `events`, `lease`, `land`, `serve-mcp`, plus
  honest stubs `lsp|duplicates|github` → "not implemented in v1".

## Bugs found and fixed this run (each has a regression test)

1. **Rust indexing silently empty** — `tree-sitter-rust` 0.24 emits grammar
   ABI 15; `tree-sitter` 0.24 accepts ≤14. Pinned `tree-sitter = "0.25"`
   (resolved 0.25.10). TypeScript worked, which masked it.
2. Claim-less changesets permanently yellow → now "unclaimed", not
   "outside claim".
3. Item-level outside touches didn't affect changeset status.
4. Joint rehearsals attributed gate verdicts to conflicted-out changesets.
5. Tower's rehearsal scratch worktree showed up in radar as a colliding
   peer; excluded from radar now.
6. Plus: full MCP tool schemas; no phantom assignment on failed
   `worktree_create`; warning-free build.

## Verified results (real numbers, this machine, git 2.43.0)

- `cargo build` and `cargo build --release`: **success, 0 warnings**.
- `cargo test`: **17 passed, 0 failed** (~4s), including the required
  acceptance tests: rehearsal leaves all user branch tips, HEADs, worktree
  files (incl. an uncommitted edit), and `git status` output byte-identical;
  a conflicting claim is refused naming the colliding assignment (A-1).
- `scripts/demo.sh` (release binary): **51 passed, 0 failed, exit 0** —
  from a fresh `init --write-config`, through "RED C-1 <-> C-2 in
  src/math.ts" with worktree 3 green, to an MCP-driven
  task→claim→changeset→rehearsal→gates→land landing on main while the red
  pair remains unresolved and unlandable.
- Hard rules held: no commits or pushes in this repo (still zero commits;
  the deliverable is the working tree), no network use, no worktrees
  touched outside demo/test scratch dirs, all run state under scratch
  `TOWER_HOME` dirs (`~/.tower` was never created).

## Stubbed or deferred

- `tower lsp|duplicates|github`: "not implemented in v1" (Phase 3).
- Watch polls (no fs watcher); integration order is creation order (no
  dependency graph); land is fast-forward only; `[reserved]`,
  `[escalation] extra_triggers`, `[limits]` parsed but not enforced;
  item-level indexing only for Rust/TypeScript. Rationale and alternatives:
  `DECISIONS.md` (13 entries).

## First commands for a human

```sh
cargo install --path .        # or: cp target/release/tower ~/.local/bin/
cd <your repo>
tower init --write-config
tower watch                   # radar board updates live; Ctrl-C to stop
tower board                   # one-shot board (add --html board.html)
scripts/demo.sh               # from this repo: the 51-assertion demo
cargo test                    # from this repo: the 17-test suite
```

To connect an assistant, point its MCP config at `tower serve-mcp`
(snippet in `README.md`).
