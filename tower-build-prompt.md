# Implementation Prompt — Build Tower

Copy everything between the lines into a fresh session with your coding agent (ChatGPT/Codex, Muse, or similar), in a session that has access to the computer it should be installed on. Attach or paste `tower-spec.md` with it. The prompt tells the agent to build the tool, install it on that machine, and start using it as the orchestration layer for implementation work in your repos.

---

You are building a new developer tool called **Tower**. The full design spec is attached to this message as `tower-spec.md`. Read it completely before writing any code. The spec is the requirements document; this prompt is your working instructions. Where the two conflict, the spec wins on *what* to build and this prompt wins on *how* to work.

## What Tower is, in one paragraph

Tower is a local tool (runs on this computer, no cloud, no accounts) that coordinates multiple coding agents working in parallel git worktrees in the same repo. It watches all worktrees in a registered repo, indexes their live diffs, continuously simulates merges between them and against main using `git merge-tree`, and maintains one board of tasks, changesets, collisions, and mergeability, all with stable IDs (T- for tasks, C- for changesets, E- for escalations, D- for decisions). Assistants talk to it through an MCP server and a CLI that mirror each other. It is language-agnostic: tree-sitter for code structure, LSP only in a later phase. Files and git remain the source of truth; Tower is a coordination layer on top and never edits anyone's worktree.

## Scope for this build

Build **Phase 1 and Phase 2** from the spec:

- Phase 1 (Radar): repo registration (`tower init`, `tower.toml`), automatic worktree discovery and watching, diff indexing at file level and item level via tree-sitter, continuous pairwise merge simulation with `git merge-tree`, green/yellow/red mergeability states with reasons, the board (CLI output plus a simple local HTML page), the event log, and MCP read tools (`board_get`, `worktree_list`, `events_since`).
- Phase 2 (Orchestrated): tasks, assignments, territory claims (grant or report exact collisions), Tower-managed worktrees, changeset registration with claimed-vs-actual diff reporting, the integration rehearsal pipeline on scratch refs (`refs/tower/*`), tiered gates from `tower.toml` (fast check on material change, full test at ready), escalation and decision objects with full MCP + CLI support, the single-orchestrator lease per repo, and `tower land` for green changesets.

Explicitly **out of scope** for this build (stub or skip, do not improvise): LSP adapters, duplicate-code detection, affected-test selection, the GitHub adapter, any hosted/multi-user features, any UI beyond the board page, Windows support.

## Technical constraints

- **Stack:** TypeScript on Node 20+. One npm package that installs a `tower` CLI. Use the official `@modelcontextprotocol/sdk` for the MCP server (stdio transport). Use `web-tree-sitter` (WASM grammars) so no native builds are required. SQLite for state via a dependency-light driver (prefer `node:sqlite` if the runtime allows, otherwise `better-sqlite3`). Shell out to the system `git` binary for all git operations; do not reimplement git.
- **State location:** per-user data dir (`~/.tower/` on macOS/Linux) with one SQLite database plus per-repo config read from `tower.toml` in the repo root. Nothing else is written into user repos except `tower.toml` (on `init`, with my approval) and scratch git refs during rehearsal.
- **Platform:** macOS first (this machine), Linux-compatible code paths, no macOS-only APIs in core logic.
- **Safety rules from the spec are hard requirements, not guidelines:** never edit, rebase, reset, or delete a user's worktree or branch; rehearsals only on Tower's own scratch refs and scratch worktrees; never land a changeset to main except on an explicit `tower land` command; every state-changing action appended to the event log with its IDs.
- **Performance:** merge simulation must be incremental and debounced (recompute only what changed). On the demo repo below, the board must reflect a new cross-worktree collision within 10 seconds of the edit that caused it.

## How to work

1. Create the project at `~/code/tower` (if that path is taken, stop and ask me). Initialize git, but do not commit unless I ask; do not push anywhere.
2. Before coding, show me a short build plan: package layout, the data model (tables), the MCP/CLI command list mapped to spec Section 11, and any spec ambiguities you found. Wait for my go-ahead on the plan. If a spec point is ambiguous and I am not available, choose the simpler interpretation and record it in `DECISIONS.md` in the project.
3. Implement in this order: state store + event log → repo registration + worktree discovery → diff indexing → merge simulation → board (CLI) → radar daemon (`tower watch`) → MCP read tools → claims + tasks → rehearsal + gates → escalations/decisions + remaining MCP tools → board HTML page → `tower land`.
4. Write tests as you go (Vitest). Every phase ends with its tests green. Do not skip the safety rules in tests: include a test proving a rehearsal leaves all user branches and worktrees byte-identical.
5. Report progress to me at each numbered step boundary in step 2's order: what was built, test result, and anything you stubbed. Keep reports short and factual.

## Acceptance demo (must pass before you call it done)

Write and run an automated demo script (`scripts/demo.sh`) that:

1. Creates a throwaway git repo with a small TypeScript program (a few files, a few functions, a trivial test suite).
2. Runs `tower init` and `tower watch` against it.
3. Creates three worktrees and makes scripted edits: worktrees 1 and 2 edit the *same* function in conflicting ways; worktree 3 edits an unrelated file.
4. Asserts the board shows worktrees 1 and 2 as a red collision naming both changesets, and worktree 3 as green.
5. Registers a task and claim through the MCP tools (drive them from a test client, not by hand), has a "worker" produce a changeset inside its claim, rehearses it, runs the gates, and lands it with `tower land`.
6. Prints PASS/FAIL per assertion and a final summary.

Also do a real-world smoke test: run Tower in radar mode against one of my existing repos with agent worktrees already in flight (ask me which repo), and show me the board output.

## Install and adopt (final step, after the demo passes)

1. Install Tower on this computer so `tower` is on my PATH (global npm install from the local package, or an equivalent you propose and I approve).
2. Start `tower watch` and show me it is running.
3. Register the smoke-test repo from the demo step.
4. Then adopt Tower as your own orchestration tool for this repo and any repo I register, from now on, following Section 16 of the spec: for every implementation task I send you in a registered repo, create the task in Tower and show me the plan receipt before starting; acquire claims per assignment; run workers in Tower-managed worktrees; register every changeset; integrate only through the rehearsal pipeline; report progress by task and changeset ID; escalate with options and a recommendation anything on the escalation list; if Tower reports a collision or a red changeset, surface it to me immediately instead of working around it silently. If Tower is not running when I send you implementation work, tell me and start it first.
5. Finish with a summary: what was built, where it lives, test and demo results, what was stubbed, and the exact commands I use day to day (`tower board`, `tower watch`, and how my assistants connect to the MCP server).

Begin by reading the spec and giving me the build plan from step 2.

---

### Notes for Aaron (not part of the prompt)

- The prompt assumes the implementing session runs on the machine Tower should be installed on (your Mac). Run it there, not in a remote sandbox.
- If the agent pushes back on `node:sqlite` availability, `better-sqlite3` is the sanctioned fallback already named in the prompt.
- After it is built, the standing instruction in spec Section 16 is the piece to paste into a repo's `AGENTS.md` so *every* future agent session in that repo uses Tower by default.
