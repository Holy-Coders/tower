# Tower — A Local Orchestration Tool for Parallel Agent Work

**Status:** Design spec v0.1 (2026-10-25)
**Codename:** Tower (working name, rename freely)
**One line:** A tool that runs on your own computer, watches every agent worktree in a repo, detects collisions and merge problems *while work is still in progress*, and gives whichever assistant is driving (ChatGPT, Muse, Codex, or a human) one organized, ID'd board of tasks that are provably mergeable when ready.

---

## 1. The problem

Today the workflow looks like this: one human, in a chat, hands out many implementation tasks. Each task runs in its own git worktree, often driven by an assistant that knows nothing about the other worktrees. The result:

- Two agents edit the same code and nobody finds out until merge time.
- A change in one worktree silently invalidates assumptions in another (a signature changed, a helper was renamed).
- By the time a conflict is discovered, the agent that caused it has lost its context, and fixing it means a large backtrack: rebase, re-solve, re-test, re-review.
- The human has no single view of what is running, what is blocked, what is ready, and what collides with what.

The failure is not agent quality. The failure is that coordination happens *after* the work, at merge, instead of *during* the work.

## 2. What Tower is (and is not)

Tower is:

- **A local tool.** It runs on the developer's computer, next to the repos and worktrees. Not a cloud service, not a GitHub app. GitHub only sees finished work; Tower exists to organize work *before* it gets there.
- **Host-agnostic.** It is not built into ChatGPT, Muse, or any one assistant. Any assistant that can call an MCP server or a CLI can use it. The intelligence (planning, decomposition, judgment) lives in the host assistant; Tower is the deterministic state machine underneath.
- **Language-agnostic.** It is not a Rust tool or a TypeScript tool. It builds its understanding of a codebase from universal tooling (tree-sitter for structure, LSP where available for semantics) and degrades gracefully per language.
- **Git-native.** Files and git remain the source of truth. Tower keeps a coordination layer on top; it never replaces the repo, and it never rewrites anyone's worktree.

Tower is not:

- Not another agent framework. It does not write code and does not ship its own models.
- Not a merge tool in the Git sense. Git merges text; Tower coordinates *work in progress* so that merges become boring.
- Not a hosted dashboard or team SaaS (at least not in v1). One developer, one machine, many agents.

## 3. Actors

- **The human.** Sits in a chat. Sends tasks in natural language, possibly many at once, possibly while others are running. Answers escalations. Can intervene in anything at any time. This is the driver.
- **The orchestrator.** The host assistant the human is talking to (ChatGPT, Muse, a future assistant). It decomposes tasks, assigns work to agents, claims territories, integrates results, runs verification, and reports back to the human with clear IDs. It decides mechanical questions itself and escalates judgment calls.
- **Worker agents.** Whatever executes a single assignment: a subagent of the host, Codex, Muse Code, or a human developer. A worker receives a worktree and a brief, and returns a changeset.
- **Tower.** The local tool. Holds all state, enforces the rules, and tells the truth about collisions, mergeability, and status. It has no opinions and no models.

## 4. Core concepts

Everything in Tower has an ID. IDs are how the human, the orchestrator, and the tool refer to the same things unambiguously in chat.

| Concept | ID | Meaning |
|---|---|---|
| Task | `T-12` | One unit of work the human asked for, as tracked from request to landed. |
| Assignment | `A-31` | One worker's piece of one task, bound to one worktree. |
| Changeset | `C-34` | The concrete diff an assignment produced, registered against its task. |
| Claim | (internal) | A lease on a territory of the codebase (files, or items within files) held by an open assignment. |
| Escalation | `E-07` | A question the orchestrator cannot or should not decide alone, waiting on the human. |
| Decision | `D-03` | A ruling by the human (or a standing instruction) that the orchestrator must honor in future planning. |
| Gate | — | A verification step (type check, tests, lint) run by Tower on an integration tree. |
| Event | — | Anything that happened, appended to one log: task created, claim conflict, gate failed, changeset landed. |

### The board

At any moment, Tower can render **the board**: every task with its state, every worktree, every known collision, every open escalation, and the current mergeability picture. The board is available as CLI output, as an MCP resource, and as a simple local HTML page. The board is the single answer to "what is going on in this repo right now."

Task states: `queued → planned → running → integrating → ready → landed`, with `blocked` (waiting on an escalation or a dependency) and `failed` as side states.

### Mergeability states

Every changeset is continuously classified:

- **Green:** merges cleanly into the integration line and passes its gates. Ready to land whenever the human says so.
- **Yellow:** merges textually, but something needs attention (a gate is failing, a dependency changeset has not landed yet, or it touches a signature another open task depends on).
- **Red:** conflicts with main or with another open changeset, or its merge rehearsal broke a gate. The board always says *which* other changeset, and *where*.

## 5. The two modes

### 5.1 Radar mode (passive) — the day-one value

Radar mode requires **no cooperation from any assistant**. Tower watches the repo on its own:

1. Discovers every git worktree automatically (`git worktree list` plus a filesystem watch), including worktrees created by tools like ChatGPT/Codex that Tower knows nothing about.
2. Indexes each worktree's live diff against its base: which files, and where parsing allows, which *items* (functions, classes, methods) changed.
3. Continuously runs simulated merges between worktrees and against main. Git can do this without touching any working tree (`git merge-tree`), so it is cheap enough to run on every change.
4. Publishes the board: collisions, coupling, and merge order, updated live.

Radar mode alone fixes the "found out too late" problem. A collision appears on the board while both workers still have their context, when the fix is cheap.

### 5.2 Orchestrated mode (active) — planning on top of detection

When the driving assistant connects to Tower (via MCP or CLI), the full loop becomes available:

1. The human sends tasks in chat.
2. The orchestrator registers them (`task.create`), decomposes them, and replies to the human with a **plan receipt**: the task IDs, the assignments under each, the territory each assignment will claim, what runs in parallel vs in sequence and why, and the assumptions it made. Assumptions are stated explicitly so the human can correct them before code exists.
3. Before an assignment starts, the orchestrator acquires **claims** on its territory. Tower grants the claim or reports exactly which open assignment it collides with. Claims are how conflicts are *prevented*, not just detected.
4. Workers execute in their worktrees. When a worker returns, its diff is registered as a changeset and Tower verifies what it actually touched against what it claimed. (Touching outside your claim is allowed, but it is reported, and it invalidates other claims' assumptions.)
5. The orchestrator integrates changesets in dependency order through the rehearsal pipeline (Section 7). Tower runs all gates itself, on its own integration tree. Workers never grade their own work.
6. The orchestrator reports events to the human by ID as they happen, and raises **escalations** for anything on the escalation list (Section 9). The human answers in chat by ID ("E-07: go with B"), the decision is logged, and work resumes.

Radar mode keeps running underneath orchestrated mode at all times, catching work done outside the system (a worktree the human started by hand, an IDE edit in a claimed area).

## 6. The map: how Tower understands a codebase

Tower builds a lightweight graph of the repo: items (functions, types, modules), the files they live in, and dependency edges (this item references that item). It builds the graph from two standard sources:

- **Tree-sitter** for structure. 100+ language grammars; gives items and syntax for almost any language with no custom parser work.
- **LSP (Language Server Protocol)** for semantics, where a language server exists: references, definitions, call hierarchy. rust-analyzer, typescript-language-server, pyright, etc. are adapters, not core.

Support is tiered, and Tower states which tier a repo is in rather than pretending uniformity:

| Tier | Available | Territory granularity | Impact analysis |
|---|---|---|---|
| 1 | Tree-sitter + LSP | Item level | Real reference graph; blast radius is trustworthy |
| 2 | Tree-sitter only | Item level claims | Fuzzy: dependents approximated by name/reference search |
| 3 | Neither | File level claims | Diff overlap only; rely on gates |

Build, check, and test commands come from per-repo configuration (Section 10), not from language assumptions.

## 7. The mergeability engine

This is the heart of Tower.

- **Continuous simulation.** On every relevant change (a worktree diff changes, main moves, a changeset registers), Tower re-runs pairwise simulated merges among open changesets and against main, using `git merge-tree`. Text conflicts are detected in near-real time at almost no cost.
- **The integration line and rehearsal.** Tower maintains a scratch integration branch (never the user's branches; scratch refs under `refs/tower/*`). Changesets marked ready are merged into it in the planned order and the configured gates run. A changeset is only ever called "ready to land" if its rehearsal passed against current main plus every changeset ahead of it in the queue.
- **Ordering.** The planned merge order is derived from dependencies: if changeset B's code references what changeset A changes, A lands first. The board shows the order and the reason for it.
- **Tiered gates, because semantic checks are not free:**
  - Tier 0 (constant): simulated text merges.
  - Tier 1 (on material diff change): fast check (e.g., type check / `cargo check` / build of affected packages) on the rehearsal tree.
  - Tier 2 (on ready): full configured test suite, or the affected-test subset when the tooling supports selecting it.
- **No surprises at land time.** Landing a green changeset is a formality (merge or PR, per repo config), because it has already been rehearsed. Landing a yellow or red changeset is possible only as an explicit human decision.

## 8. Feedback: how the human experiences it

The orchestrator, powered by Tower's event log, speaks a small, consistent language:

- **Plan receipt** when tasks are accepted: IDs, assignments, parallelism, assumptions.
- **Event notes** at natural beats only: assignment started, changeset registered, gate failed (with the failing gate and the changeset ID), collision detected (with both changeset IDs), changeset ready, task landed.
- **Escalation packets** in a fixed format:

> **E-07** (for T-12): Agent A-31 changed the signature of `parse_invoice`, which A-33 is coding against.
> **Options:** A) A-33 rebases onto the new signature now (10 min rework). B) Land A-31 last, A-33's version of the signature wins, A-31 adjusts.
> **Recommendation:** A, because two other open tasks also use the new signature.
> **Paused:** A-33 only. Everything else continues.

- **Status on demand:** "status" returns the board in chat form.
- The human can act on anything by ID: approve, kill (`kill T-12`), reprioritize, answer an escalation, or revoke a decision (`revoke D-03`).

## 9. Escalation policy

The orchestrator decides mechanical questions itself: disjoint edits, rename-only changes, gate failures with an obvious owner, re-sequencing around a busy territory.

The orchestrator **must escalate** (defaults; per-repo config can extend, never silently shrink, this list):

- A change to the signature of a public/exported item that other open work depends on.
- Adding a new third-party dependency.
- Introducing `unsafe` code, or touching auth, crypto, payments, or PII-handling areas.
- A changeset that deletes or rewrites code owned by another open task.
- Two changesets whose intents genuinely conflict (both pass gates, but they embody different product decisions).
- A gate that fails three repair attempts by the owning worker.
- Any change to reserved territory: macro definitions, code generators, database migrations, build scripts, lockfiles (beyond what a dependency change implies).

Design target: escalations should be rare and cheap to answer. If the human is getting more than a couple per task on average, the orchestrator is planning badly; Tower's log makes that measurable.

Decisions are remembered. A decision (D-ID) becomes a standing instruction: Tower returns relevant decisions at planning time and the orchestrator must conform or re-escalate. Decisions never become invisible law; the human can list and revoke them at any time.

## 10. Configuration (per repo)

A single file in the repo root, `tower.toml`:

```toml
[repo]
main_branch = "main"

[gates]
check = "cargo check --workspace"        # tier 1
test  = "cargo test --workspace"         # tier 2
# test_affected = "cargo test -p {package}"  # optional

[reserved]
paths = ["**/migrations/**", "build.rs", "**/*.lock"]
patterns = ["macro_rules!", "proc_macro"]

[escalation]
extra_triggers = ["**/billing/**"]

[limits]
max_concurrent_assignments = 4
```

No config is required to start: Tower infers a default check command where it can (Cargo, npm, go, etc.) and runs in radar mode with Tier-0 simulation only until gates are configured.

## 11. Tool surface

Tower is one local binary/daemon with state in a per-user data directory (SQLite; nothing in the repo except `tower.toml` and scratch git refs). Two interfaces expose the same operations:

### CLI (for humans and scripts)

```
tower init                # register this repo, write default tower.toml
tower board               # print the board
tower watch               # run the radar daemon for this machine
tower task list|create|show|kill
tower merge-check         # run simulation + rehearsal now, print results
tower land C-34           # land a green changeset (merge or PR per config)
tower decisions list|revoke
tower serve-mcp           # expose the MCP server (stdio)
```

### MCP tools (for assistants)

- `board_get(repo)` — full board state.
- `task_create(repo, title, brief)` → `T-ID`
- `task_plan_save(repo, task_id, plan)` — store the orchestrator's decomposition and assumptions.
- `claim_acquire(repo, assignment_id, nodes)` → granted, or the exact collisions.
- `claim_release(repo, assignment_id)`
- `worktree_create(repo, task_id)` / `worktree_list(repo)` — Tower-managed worktrees under a known directory.
- `changeset_register(repo, assignment_id, worktree)` → `C-ID`, with claimed-vs-actual diff report.
- `changeset_status(repo, changeset_id)` — green/yellow/red with reasons.
- `integration_order(repo)` — planned landing order with dependency reasons.
- `gate_run(repo, changeset_id, tier)` — run gates on the rehearsal tree; returns attributed results.
- `escalate_create(repo, task_id, question, options, recommendation)` → `E-ID`
- `escalate_resolve(repo, escalation_id, answer)` — records the human's answer.
- `decision_list(repo)` / `decision_log(repo, text)` → `D-ID`
- `events_since(repo, cursor)` — the event log stream the orchestrator turns into chat feedback.

The CLI and MCP must be exact mirrors; anything a human can see, an assistant can see, and vice versa.

## 12. Safety rules (non-negotiable)

1. Tower never edits a worker's worktree, never rebases or resets user branches, and never deletes a worktree or branch. Rehearsals happen only on Tower's own scratch refs and scratch worktrees.
2. Tower never lands a changeset to main without an explicit instruction (from the orchestrator acting on the human's standing instruction, or the human directly).
3. A claim is a coordination signal, not a lock on the filesystem. Workers *can* write anywhere; Tower detects and reports it. Enforcement is by visibility, not by permission bits.
4. All state is local. No code, diffs, or metadata leave the machine except through git remotes the human already uses (and only when landing via PR).
5. Every action Tower takes is in the event log, attributable to an ID.

## 13. Limitations and honest edges

- **Semantic compatibility is not decidable.** Tower prevents most conflicts by partitioning and detects textual and gate-level conflicts early. Two changes can still be individually correct and jointly wrong in a way no test catches. The mitigation is the escalation path to a human who knows the intent, not a smarter merge.
- **Graph quality varies by language.** Tier-1 repos get precise territories; Tier-3 repos get file-level coordination. Tower must label the tier honestly and the orchestrator must widen claims on lower tiers.
- **Macros, codegen, and build scripts are opaque.** Tree-sitter sees the invocation, not the expansion. Hence reserved-territory treatment and serialization.
- **Dynamic languages blur references.** Renames and signature changes in Python/JS may not show all dependents in the graph; gates carry more weight there.
- **Duplication is only partly detectable.** Two workers writing equivalent helpers under different names produce no conflict. Tower keeps a registry of newly created items and flags likely duplicates (same signature shape / similar body), and the orchestrator can assign a consolidation task. This is a heuristic net, not a proof.
- **Verification cost.** Tier-2 gates on large repos are expensive. Tower budgets them (only at ready, affected subsets where possible) and reports gate cost per task so the human can see what parallelism costs.
- **Host quality varies.** Tower guarantees state and detection, not judgment. A weak orchestrator will still plan badly; Tower's contribution is that the bad plan is visible on the board before it becomes expensive, and every consequence is attributable to an ID.
- **One orchestrator per repo at a time.** Two hosts driving the same repo concurrently would fight over claims and integration order. Tower enforces a single active orchestrator lease per repo; a second host can observe (radar/board) but not plan until the lease is released.

## 14. Build phases

**Phase 1 — Radar (the wedge).**
Repo registration, worktree discovery and watching, diff indexing (file level + tree-sitter item level), pairwise `git merge-tree` simulation, mergeability board (CLI + static HTML), event log, MCP read tools (`board_get`, `worktree_list`, `events_since`). No claims, no landing. Success criterion: point it at a repo where ChatGPT/Codex are running three worktrees; the board shows a same-file collision at least an hour before a human would have found it at merge time.

**Phase 2 — Orchestrated mode.**
Tasks/assignments/claims, plan receipts, changeset registration with claimed-vs-actual reporting, integration line and rehearsal, tiered gates from `tower.toml`, escalation and decision objects and their MCP tools, single-orchestrator lease, `tower land`. Success criterion: one human in one chat runs four parallel tasks in one repo end to end; zero merge surprises; every escalation answered by ID.

**Phase 3 — Depth.**
LSP adapters (references/blast radius), duplicate-item detection, affected-test selection, GitHub adapter (CI status as a gate, PR creation for green changesets, merge queue landing), gate cost reporting.

## 15. The standing usage pattern (the point of it all)

Once installed, the intended daily flow is:

1. `tower watch` runs on the machine (login service or on demand).
2. The human opens a chat with any Tower-connected assistant and just sends work, in that repo, in natural language.
3. The assistant's standing instructions (Section 16) make it use Tower for *every* implementation task in a registered repo: register the task, show the plan receipt, claim, dispatch, integrate, report by ID, escalate when required.
4. The human's whole job is sending tasks and answering the occasional E-ID.

## 16. Standing instruction snippet (for AGENTS.md / assistant instructions)

> This repo is registered with Tower, the local worktree orchestration tool. For every implementation task in this repo, use Tower: create the task and show me the plan receipt before starting work; acquire claims for each assignment's territory; run workers in Tower-managed worktrees; register every changeset; integrate only through the rehearsal pipeline; report progress by task and changeset ID; and escalate, with options and a recommendation, anything on Tower's escalation list. If Tower reports a collision or a red changeset, surface it to me immediately rather than working around it silently. If Tower is not running, tell me and start it (`tower watch`) before doing implementation work.

---

*End of spec v0.1.*
