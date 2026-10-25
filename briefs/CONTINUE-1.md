# Tower build — continuation run 1

You are continuing a build that was interrupted by a machine restart about 8
minutes in. The original brief is `briefs/BUILD-BRIEF.md` in this repo —
read it first; it remains the governing spec for scope, stack, stages, and
hard rules.

## State left by the interrupted run
- Source files exist and were being written in stage order: `Cargo.toml`,
  `Cargo.lock`, `src/config.rs`, `src/store.rs`, `src/git.rs`,
  `src/index.rs`, `src/engine.rs`, `src/board.rs`, `src/mcp.rs`,
  `src/ops.rs`, `src/main.rs`. A `cargo build` had started (`target/debug`
  exists). The last edits were appends to `src/engine.rs` and
  `src/main.rs`, so those files may be incomplete or not yet compiling.
- No `PROGRESS.md` was written yet. No tests have been verified. No demo
  script exists yet.

## Your job
1. Assess before you rewrite: run `cargo build` and read the errors; read
   the existing sources enough to know what is complete and what is stubbed
   or missing. Prefer completing/fixing what exists over rewriting it.
2. Finish the build per the original brief: all stages (data layer, git
   adapter + simulated merges, live watch + board, claims/tasks/escalations,
   tree-sitter map, gates + landing, MCP server, CLI), `cargo test` green,
   `scripts/demo.sh` implemented and passing, README written.
3. Write `PROGRESS.md` now and keep it updated at every stage boundary:
   what is done, what remains, test counts. If you get interrupted again,
   the next run resumes from it.
4. Write `reports/final-summary.md` at the end: what was built, final
   test/demo results with real numbers, anything stubbed or deferred, and
   the first commands a human would run.
5. Hard rules from the original brief still apply: no commits, no pushes,
   no network publishing, never touch worktrees outside the demo's own
   scratch repos, all state under `~/.tower/`.
