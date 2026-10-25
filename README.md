# Tower

Tower is a local orchestration tool for parallel agent work in git worktrees.
It runs on your own machine, watches every worktree in a registered repo,
continuously simulates merges between them and against main, and keeps one
board of tasks (T-), assignments (A-), changesets (C-), escalations (E-), and
decisions (D-) that are provably mergeable before anything lands. Assistants
talk to it through a CLI and a mirroring MCP server; files and git remain the
source of truth. Tower never edits, rebases, resets, or deletes a user
worktree or branch.

This build implements Phase 1 (Radar) and Phase 2 (Orchestrated) of
[tower-spec.md](tower-spec.md)
in Rust, as a single `tower` binary, plus the Phase 2.1 playtest fixes:
order-aware statuses, Tower-owned landing candidates, gate proofs bound to
their tree, changeset refresh/supersede, partial claim grants with
per-node release, and board next-actions. Phase 2.2 adds the
deep-playtest fixes: claims survive landing (they belong to the
assignment), each Rust test function is indexed as its own item,
outside-claim detail is named wherever the board or `changeset status`
prints its hint, and `changeset conflicts` shows merge-tree hunk headers
without leaving Tower.

## Install

Requires a stable Rust toolchain and the system `git` binary. The
one-shot installer does everything — prerequisites (rustup if cargo is
missing), build + install, PATH setup, an end-to-end smoke test, and
MCP registration for agents — and is safe to re-run after updates:

```sh
bash scripts/install.sh
# by hand instead: cargo install --path .
# or: cargo build --release && cp target/release/tower ~/.local/bin/
```

State lives in `~/.tower/` (SQLite database, board HTML, Tower-managed and
rehearsal worktrees). Set `TOWER_HOME` to override. The only thing Tower
writes into a user repo is `tower.toml` (on `init`, and only with
`--write-config` or an interactive yes) plus scratch refs under
`refs/tower/*` during rehearsal.

## Quickstart

```sh
cd your-repo
tower init --write-config   # register repo; write a default tower.toml
tower watch                 # radar loop: scans all worktrees every 2s
tower board                 # print the board (also: --json, --html file)
```

`tower watch` is a polling loop (default 2s; `--interval`, `--once`,
`--iterations N` for bounded runs). It discovers worktrees automatically,
indexes each worktree's diff vs its merge-base (files, plus tree-sitter
items for Rust and TypeScript), simulates pairwise merges with
`git merge-tree`, writes a static HTML board under `~/.tower/`, and appends
events on collisions and material diff changes (`--run-tier1` also runs the
Tier-1 gate rehearsal on material change).

Orchestrated flow:

```sh
tower task create "Add invoice export" --brief "..."          # -> T-1
tower task plan-save T-1 --plan "..."
tower worktree create --task T-1                              # -> A-1, worktree under ~/.tower/worktrees/
tower claim acquire --assignment A-1 --node src/invoice.ts#export_invoice
# ... worker edits and commits in that worktree ...
tower changeset register --assignment A-1 --worktree <path>   # -> C-1 + claimed-vs-actual report
tower gate run C-1 --tier 1 && tower gate run C-1 --tier 2    # rehearsal-tree gates
tower changeset status C-1                                    # green/yellow/red + next + claimed-vs-actual
tower changeset refresh C-1                                   # recompute CVA/head live, bump version
tower changeset conflicts C-1                                 # conflicting files + merge-tree hunk headers (read-only)
tower land C-1                                                # fast-forward or Tower-built candidate (below)
```

Other commands: `tower merge-check` (simulate + rehearse everything now),
`tower order`, `tower escalate create|resolve|list`,
`tower decisions list|log|revoke`, `tower events --since <seq>`,
`tower lease acquire|release|status` (single-orchestrator lease per repo),
`tower task list|show|kill`, `tower assignment list|create|close`,
`tower claim release|list`, `tower worktree list`,
`tower changeset refresh|supersede|conflicts [--json]`. All commands accept a global
`--repo <path>` (default: current directory's repo root).

### Statuses are order-aware

Changesets are green when they merge cleanly and every configured gate has
a current pass, yellow while gates are pending/stale or work touched
outside its claim, and red on blocking merge conflicts (naming the other
changeset and files) or a failing gate. A conflict blocks only against
main and against *earlier* unlanded changesets in integration order; a
conflict with a *later* changeset is recorded informationally ("blocks
C-3 in src/x.rs") and does not redden the earlier one, so a conflicting
set can land sequentially in order. Every changeset on the board (text,
JSON, and HTML) carries a `next` hint derived purely from its
status/reasons: `land`, `run gates`, `rerun gates (proof stale)`,
`resolve conflict with C-2 in src/x.rs`, `refresh claims or changeset`,
`superseded`, or `landed`. An outside-claim yellow names exactly what is
outside on its reason line — `touched files outside claim:
src/orders.rs#discount_is_tenth, src/notify.rs (whole file)` — in the text
board, in `tower changeset status` (whose JSON also carries
`claimed_vs_actual`, the same shape as `board --json`, with `outside_items`
as `{"file", "item"}` objects), and in the MCP `changeset_status` tool, so
the "refresh claims" hint always says what to claim. A repo with no gates
in `tower.toml` runs Tier-0 simulation only, and the board says so.

### Gate proofs and staleness

Every gate verdict is recorded with its proof context: the rehearsal or
candidate tree hash (`HEAD^{tree}`), the main head at gate time, and the
participant snapshot commits that went into that tree. A pass stops
counting when any of those move — the changeset's own snapshot, a
participating peer's snapshot, or main — and the changeset drops to
yellow ("gate proof stale (source, peer, or main changed) — rerun
gates") until the gates run again. Verdicts recorded before Phase 2.1
(no proof fields) are grandfathered as current.

### Landing: fast-forward or Tower-built candidate

`tower land <C-id>` lands committed heads only and refuses red
changesets, dirty worker/main worktrees, and yellow changesets blocked
for non-gate reasons (e.g. outside-claim). Green changesets that
fast-forward main still land by fast-forward. Otherwise — including a
yellow whose only blockers are pending/stale gate proofs — Tower builds
a *landing candidate* in its own scratch worktree (separate from the
rehearsal scratch, never the worker's): it resets to the current main
head, merges the changeset's committed head, and runs the configured
gates on that candidate tree. A merge conflict aborts, marks the
changeset red with the conflicted files, and leaves main and the worker
untouched; a gate failure marks it red with the gate output. Before
advancing, Tower re-reads both heads and rebuilds the candidate once if
either moved (twice = fail, retry). Main then advances only to the
candidate head — `--ff-only` in the checked-out main worktree when clean,
else a compare-and-swap `update-ref`. The `changeset_landed` event
records the source head, candidate head, the main head it was based
on, and `claims_held` — the assignment's claims still held after the
landing. The `tower land` receipt carries the same `claims_held` list:
landing never releases claims (see below), so a continuing assignment's
next changeset is not surprised by territory it legitimately claimed in
an earlier round suddenly counting as outside-claim.

### Changeset refresh and supersede

Claimed-vs-actual is recomputed from *live* claims and the *live* diff on
every status refresh, so expanding claims can clear an outside-claim
yellow without re-registering. `tower changeset refresh <id>` forces
that recomputation, records the new head, and bumps the changeset's
`version` (`changeset_refreshed` event). Re-registering an assignment
supersedes its previous unlanded changesets, landing a changeset
supersedes its task's other unlanded changesets, and
`tower changeset supersede <id>` does it manually. Superseded changesets
leave the integration order, status refresh, radar pairing, and gate
attribution, and are listed on the board in a short SUPERSEDED section
(id, superseded-by) instead of the main list.

### Claims: partial grants, per-node release

`tower claim acquire` grants the uncontested subset of a bundle and
reports the rest: the JSON keeps `granted` (true only when everything was
granted) and adds `granted_nodes` and `refused` (each naming file, item,
and the holding assignment). The CLI exits 1 only when zero nodes were
granted. `tower claim release --assignment A-1 --node file#item`
(repeatable) releases exactly those nodes; without `--node` it releases
all of the assignment's claims. Claims belong to the *assignment*, not
to any one changeset: landing a changeset never releases them. They are
released only by an explicit `claim release` or by `tower assignment
close <id>` (which releases all of that assignment's claims; `task kill`
releases them the same way, by closing its assignments).

### Rust test functions are individual items

The Rust indexer walks inline modules, so each function inside a
`#[cfg(test)] mod tests` is indexed as its own item, named by the
function — there is no collapsed `file#tests` item. A claim on
`src/orders.rs#discount_is_tenth` covers edits to that test (and only
that test); a whole-file claim on `src/orders.rs` covers all of them.
Claim coverage is file-scoped throughout: a claim held in one file never
covers a same-named item in another.

### Conflict detail without raw git

`tower changeset conflicts <id>` is read-only: for each file the
changeset conflicts in — against main and/or earlier unlanded
changesets, the same set that makes its status red — it prints the file,
the counterparty, and the conflicting hunk headers (`@@` lines) from
`git merge-tree`, e.g.:

```text
C-3 conflicts:
  src/orders.rs (vs C-2)
    @@ -1,3 +1,3 @@
```

`--json` prints the structured form (`{"changeset_id", "conflicts":
[{"against", "file", "hunks"}]}`), which is also the MCP
`changeset_conflicts` tool result. A non-conflicted (including landed
or superseded) changeset reports an empty conflict list and exits 0.
Nothing is resolved and no worktree is written; fixing the conflict
stays in the worker's worktree.

## Connect an assistant (MCP)

`tower serve-mcp` is a stdio MCP server (JSON-RPC 2.0, line-delimited)
exposing all 16 spec tools plus Phase 2.1's `changeset_refresh` and
`changeset_supersede` and Phase 2.2's `changeset_conflicts` (19 total):
`board_get`, `task_create`,
`task_plan_save`, `claim_acquire`, `claim_release`, `worktree_create`,
`worktree_list`, `changeset_register`, `changeset_status`,
`changeset_refresh`, `changeset_supersede`, `changeset_conflicts`,
`integration_order`,
`gate_run`, `escalate_create`, `escalate_resolve`, `decision_list`,
`decision_log`, `events_since`. `claim_acquire` returns the partial-grant
fields (`granted_nodes`, `refused`) and `claim_release` takes an optional
`nodes` array for per-node release. `changeset_status` includes
`claimed_vs_actual` (same shape as `board_get`), and
`changeset_conflicts` returns the read-only conflict detail above. Every
tool takes `repo` (path to a registered repo).
Example MCP client configuration:

```json
{
  "mcpServers": {
    "tower": {
      "command": "tower",
      "args": ["serve-mcp"]
    }
  }
}
```

## Configuration (`tower.toml`)

```toml
[repo]
main_branch = "main"

[gates]
check = "cargo check"   # tier 1
test  = "cargo test"    # tier 2

[reserved]
paths = ["**/migrations/**", "build.rs", "**/*.lock"]
patterns = ["macro_rules!", "proc_macro"]

[escalation]
extra_triggers = []

[limits]
max_concurrent_assignments = 4
```

`reserved` and `limits` are parsed and shown but not enforced in v1 (see
limitations below); gates are what drive green/yellow/red beyond Tier-0.

## Verify your build

```sh
cargo test                # 37 tests, incl. candidate-landing, staleness, supersede, claims-on-land, conflict detail
scripts/demo.sh           # 6-step acceptance demo, PASS/FAIL per assertion (51)
scripts/demo-phase21.sh   # sequential candidate landing + refused conflict (47)
scripts/demo-phase22.sh   # claims survive landing, per-test-fn claims, conflict hunks (60)
```

## Known limitations / stubs

- **Out of scope, stubbed**: `tower lsp`, `tower duplicates`, `tower github`
  print "not implemented in v1". No LSP adapters, duplicate detection,
  affected-test selection, GitHub adapter, or Windows support.
- **Watch** polls every 2s (no filesystem watcher in v1).
- **Integration order** is changeset creation order; there is no
  item-reference dependency graph yet (Phase 3 depth), so reasons say so.
- **Landing** is fast-forward or Tower-built candidate (see above):
  candidates only ever land committed heads through a Tower-owned scratch
  worktree, and conflict resolution inside a candidate is not attempted —
  a conflicting candidate is refused with the conflicted files named.
  `changeset conflicts` shows the conflicting hunk *headers* read-only;
  it does not print full diffs or attempt any resolution, and a
  non-content conflict (e.g. modify/delete) names the file with no `@@`
  headers to show.
- **Item-level indexing** covers Rust and TypeScript items (functions,
  types, impls/classes, methods — including Rust functions inside inline
  modules, notably each test function in a `#[cfg(test)]` module);
  other languages are file-level. TypeScript callback-style tests
  (`describe`/`it`) are not indexed as items, so a TS file changed only
  in test callbacks is tracked at file granularity. Claims and impact
  degrade to file granularity wherever no items are indexed.
- **`[reserved]`, `[escalation] extra_triggers`, `[limits]`** are parsed
  but not enforced; escalations are created explicitly via CLI/MCP.
- **`cargo install --path .`** is not run by the demo or tests; the demo
  uses `target/release/tower` (override with `TOWER_BIN`).
- Radar simulation of *uncommitted* work uses a temporary git index to make
  a read-only snapshot commit object; user branches, index, and files are
  never modified (covered by the safety test).
