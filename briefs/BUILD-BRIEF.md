# Build Brief — Tower, Rust implementation

You are implementing **Tower**, a local orchestration tool for parallel agent work in git worktrees. You are running on Aaron's Mac; your working directory is the project root, `~/code/holy-coders/tower`. Two documents in the root are your requirements, read them fully before writing code:

- `tower-spec.md` — the design spec (what to build, concepts, tool surface, safety rules, phases). This wins on WHAT.
- `tower-build-prompt.md` — working instructions written for a TypeScript implementation. Follow its *process* (plan first, phase order, tests, acceptance demo, progress reporting), but its stack section is OVERRIDDEN: this implementation is in **Rust**, per the notes below.

## Stack override (Rust)

- Rust, single cargo project producing one binary named `tower`. Edition 2021, stable toolchain. If cargo is not on PATH, install stable Rust via rustup first: `curl --proto '=https' --tlsv1.2 -sSf https://sh.rustup.rs | sh -s -- -y --default-toolchain stable`, then `source ~/.cargo/env`.
- CLI parsing: `clap` (derive). State: SQLite via `rusqlite` with the `bundled` feature, stored under `~/.tower/` (per-user data dir; repo config read from `tower.toml` in the repo root, parsed with `serde` + `toml`).
- All git operations by shelling out to the system `git` binary (`std::process::Command`). Do not use a git library.
- Code structure indexing: `tree-sitter` crate with at minimum the Rust and TypeScript grammars (`tree-sitter-rust`, `tree-sitter-typescript`); any other language falls back to file-level granularity. Extract top-level items (functions, types, impls, classes, methods where the grammar makes it natural) with their name, kind, file, and line range.
- MCP server: stdio transport, JSON-RPC 2.0 line-delimited. Use the `rmcp` crate if it works cleanly; if it fights you, implement the small subset by hand (initialize, tools/list, tools/call) — the tool list in spec Section 11 is the contract, not the framework.
- Tests: `cargo test`. Demo: `scripts/demo.sh` (bash) as specified in the build prompt, adapted to the Rust binary and a Rust or TypeScript demo program.

## Scope

Build Phase 1 and Phase 2 from spec Section 14:

- Phase 1 (Radar): repo registration (`tower init` writing a default `tower.toml` only when approved by a `--write-config` flag or interactive yes — in tests use the flag), automatic worktree discovery (`git worktree list --porcelain`) plus a watch loop (poll every 2s; a `notify`-crate filesystem watcher is welcome but polling is acceptable if documented), diff indexing per worktree vs its merge-base with main (files changed + items changed), continuous pairwise merge simulation with `git merge-tree` (use `git merge-tree --write-tree --merge-base=<base> A B` form; parse conflict output), green/yellow/red classification with reasons, the board as CLI output plus a generated static HTML page, the append-only event log, MCP read tools (`board_get`, `worktree_list`, `events_since`).
- Phase 2 (Orchestrated): tasks (T- IDs), assignments (A- IDs) bound to worktrees, territory claims at item or file granularity (grant, or return the exact conflicting assignment), Tower-managed worktree creation under `~/.tower/worktrees/<repo-hash>/`, changeset registration (C- IDs) with claimed-vs-actual diff comparison, integration rehearsal on scratch refs under `refs/tower/*` using a scratch worktree, tiered gates from `tower.toml` (tier 1 check command on material diff change, tier 2 test command at ready), escalations (E- IDs) and decisions (D- IDs) with full CLI+MCP support, single-orchestrator lease per repo, and `tower land` (fast-forward merge of a green changeset's branch into main, refusing non-green ones).

Out of scope (stub commands may exist but must say "not implemented in v1"): LSP adapters, duplicate detection, affected-test selection, GitHub adapter, Windows.

## Hard rules

- Safety rules in spec Section 12 are non-negotiable and tested: never edit/rebase/reset/delete a user worktree or branch; rehearsals only on scratch refs/worktrees; land only on explicit command; every state change appends an event.
- IDs are sequential per repo with the documented prefixes and are stored, never reused.
- No commits and no pushes: leave the git history of THIS project alone (git init it if it is not a repo yet); your deliverable is the working tree. (Creating the demo's throwaway repos in /tmp during tests is fine.)
- If a spec point is ambiguous, pick the simpler interpretation and record it in `DECISIONS.md` in the project root.
- Keep the code honest about tiers: a repo with no configured gates runs Tier-0 simulation only and the board says so.

## Acceptance (must all pass before you finish)

1. `cargo build --release` succeeds; `cargo test` is fully green, including a safety test proving a rehearsal leaves all user branches and worktree files byte-identical, and a claims test proving a conflicting claim is refused with the colliding assignment named.
2. `scripts/demo.sh` implements the 6-step demo from the build prompt (adapting item 5 to drive MCP via a small test client you write, e.g. a Rust integration test or a shell script piping JSON-RPC into `tower serve-mcp`): conflicting worktrees flagged red naming both, unrelated worktree green, then task → claim → changeset → rehearsal → gates → land. It prints PASS/FAIL per assertion.
3. A README.md in the project root: what Tower is, install (`cargo install --path .`), quickstart (`tower init`, `tower watch`, `tower board`), the MCP connection snippet for an assistant, and known limitations/stubs.

## Process

Work in this order: data model + state store + event log → repo registration + worktree discovery → diff indexing → merge simulation → board CLI → watch loop → MCP read tools → tasks/claims/worktrees → rehearsal + gates → escalations/decisions + remaining MCP tools → land → demo + README. Leave a short `PROGRESS.md` updated at each stage boundary (stage, what works, test status, stubs), since the human will read it after. Your final message must summarize: what was built, build/test/demo results with real numbers, what was stubbed, and any DECISIONS.md entries that matter.

## Logging, monitoring, and direction (mandatory)

- Dobby (Aaron's Muse assistant) monitors this build by reading this project's files on this Mac. He has no shell here; your logs are his only window. Keep `PROGRESS.md` current at every stage boundary (stage, what works, test status with real numbers, stubs). Write `reports/stage-<n>-report.md` for every stage (Stage 0 is the build plan), each containing what was built, the exact commands run, the raw tail of the test output (real numbers, not a paraphrase), anything stubbed, and any questions or blockers. Keep `DECISIONS.md` current.
- At the start of any run, read `PROGRESS.md`, `briefs/DIRECTION.md`, and the latest stage report, and resume at the first unfinished stage. Never redo a stage that `PROGRESS.md` marks complete unless `DIRECTION.md` tells you to.
- At every stage boundary, re-read `briefs/DIRECTION.md`. A standing GO for all stages lives there; if a STOP or a Corrections section has appeared, it overrides the GO: apply it before starting new work.
- Runs are one-shot: if a run ends mid-build, the next launch of the same command continues from `PROGRESS.md`. Make sure `PROGRESS.md` always states the last completed stage and the next stage to run, so a fresh run can pick up cleanly.
