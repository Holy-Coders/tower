# Tower

Licensed under the [MIT License](LICENSE).

Tower is a local orchestration tool for parallel agent work in git worktrees.
It runs on your own machine, watches every worktree in a registered repo,
continuously simulates merges between them and against main, and keeps one
board of tasks (T-), assignments (A-), changesets (C-), escalations (E-), and
decisions (D-) that are provably mergeable before anything lands. Assistants
talk to it through a CLI and a mirroring MCP server; files and git remain the
source of truth. Tower never edits, rebases, resets, or deletes a user
worktree or branch.

Version 0.3.0 adds opt-in automatic dispatch: declared task dependencies,
durable local agent supervision, cooperative reconciliation handoffs, and
a FIFO queue for shared gate resources. Existing manual workflows keep
working. See [CHANGELOG.md](CHANGELOG.md) for release changes.

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

## Automatic scheduling

Ordinary task dispatch is opt-in per task. Configure a local executable in `tower.toml`
or in that task's schedule specification. Tower runs the executable with
the configured argument vector, sends the task brief and plan on stdin,
and uses the assigned worktree as its working directory. It does not
change the executable's authentication or approval policy.

For an installed Codex CLI, one configuration is:

```toml
[scheduler.agent]
program = "codex"
args = ["exec", "--json", "--sandbox", "workspace-write", "-"]
timeout_secs = 1800
```

Other local agents or wrappers can use the same stdin/process-exit
contract. Use a wrapper when the agent needs a different input protocol.
The configured program must already be installed and authorized to work
on the tasks being scheduled.

After creating and planning tasks, save a JSON specification for each
task to opt it in. For example, `export.schedule.json` for `T-2`:

```json
{
  "depends_on": ["T-1"],
  "nodes": [{"file": "src/invoice.ts", "item": "export_invoice"}],
  "assumptions": ["The export interface remains compatible."],
  "acceptance_tests": ["npm test -- invoice-export"],
  "max_attempts": 2
}
```

```sh
tower schedule configure T-2 --spec export.schedule.json
tower schedule ready
tower schedule tick --holder local-coordinator
# Or poll, with an optional bounded number of iterations:
tower schedule run --holder local-coordinator --interval 2 --iterations 100
tower schedule status
tower schedule cancel T-2
```

Only declared dependencies are considered, and a dependency is satisfied
when its task has landed. Readiness names missing prerequisites, failed
or cancelled dependencies, open assignments, conflicting claims,
unfinished reconciliation handoffs, exhausted attempts, and missing agent
configuration. Cycles and unknown dependencies are rejected when saving
a schedule. Claims are repo-relative files with optional item names;
`max_attempts` is between 1 and 5 and defaults to 1.

Each coordinator iteration uses the existing orchestrator lease. Open
ordinary assignments, including manually created ones, count against
`limits.max_concurrent_assignments` (default 4). Designated reconciliation
tasks use a separate `limits.max_concurrent_reconciliations` allowance
(default 1), so waiting participants cannot consume the slot needed to
reconcile their shared work. Tower creates a managed
worktree and assignment, acquires the requested scope, and durably
reserves an attempt before launch. The run directory is a permanent
launch key. A supervisor records heartbeat, stdout/stderr, and completion
under `TOWER_HOME/agents/`; restarting the coordinator observes that
attempt instead of starting it again. Failed or timed-out attempts may
retry within their budget in fresh worktrees; earlier drafts stay in
place.

An agent's successful exit starts verification. Tower requires a clean,
committed result, registers its changeset, and runs configured gates.
The result then needs review and explicit `tower land`; completing a
process does not mark the task landed. Close completed assignments to
release their claims and scheduling slots. No agent result is
automatically published or merged.

Cancellation is a request to the owning supervisor, which stops its agent
process group. Claims and reservations stay held until a terminal receipt
confirms termination. Missing or stale receipts produce `uncertain`;
Tower holds ownership for inspection and does not spend another attempt
or signal a saved PID. Inspect the reported run directory before resolving
such an interruption. When retiring a coordinator, release its lease
with `tower lease release --holder local-coordinator`.

## Cooperative reconciliation

When two assignments need to change the same implementation,
`tower reconcile discover` reports structural overlaps. It does not
decide whether their intended behaviors agree. A configured coordinator
can propose one handoff from both stored task briefs/plans and their
schedule's authored assumptions and acceptance commands. Missing
acceptance commands keep the handoff waiting; participants can supply
them through the `intent` action. You can also request a reconciliation
explicitly with both agents' requirements and shared nodes, for example
in `handoff.json`:

```json
{
  "participants": [
    {
      "assignment_id": "A-1",
      "intent": "Preserve the existing retry limit.",
      "assumptions": ["Retry counts are nonnegative."],
      "acceptance_tests": ["cargo test retry_count_stays_capped"]
    },
    {
      "assignment_id": "A-2",
      "intent": "Record each retry's settlement result.",
      "assumptions": ["Settlement tracking does not start extra retries."],
      "acceptance_tests": ["cargo test each_retry_records_one_settlement"]
    }
  ],
  "nodes": [{"file": "src/shared.rs"}]
}
```

```sh
tower reconcile request --input handoff.json
tower reconcile list
```

The returned `R-` record pins both drafts and creates a reconciliation
task. Each participant commits its preserved draft and acknowledges a
pause of the shared scope through `pause`. Independent work can continue.
Authored `assumptions` and `acceptance_tests` travel with each intent into
the reconciliation brief. Each participant must supply nonempty
acceptance commands before dispatch. Tower executes both sets against
the combined result and the participant's own commands again during
downstream resume, recording pass/fail receipts bound to the exact head,
tree and contract version. Commands must exercise the intended behavior;
the example names above refer to tests supplied by the target project.
An optional `contract` records an authored common interface and an exact
provisional stub commit; both participants must acknowledge the current
version through `stub-ack`. Until then, dependent work stays paused.
Tower does not invent the contract or install the stub in user worktrees.

Once handoffs are ready, the scheduled reconciler receives both intents,
immutable draft commits, and any contract. Only the shared scope transfers
to that assignment. Its combined result must stay within scope and pass
at least one configured validation gate with current proof. The local
`publish` action returns the verified result commit to the participants;
it does not publish anything externally. Scheduler verification performs
this step for a scheduled reconciliation result.

Participants merge the returned commit in their own worktrees, rerun
their downstream gates, and acknowledge with `resume`. The reconciliation
completes only after both validate. Landing checks prevent an unresolved
handoff or provisional stub from counting as finished work. Cancellation
after dispatch or a provisional contract holds the reservation for
explicit recovery.

Every reconciliation action uses `tower reconcile ACTION --input PATH`
with a JSON object; `discover` and `list` need no input. Agents must poll
Tower events and act on handoffs themselves. See
[RECONCILIATION.md](RECONCILIATION.md) for action fields, state transitions,
and a full example.

The `metrics` action takes `{"id": "R-1"}` and reports state durations,
contract versions, acceptance failures and receipts. State timestamps use
whole seconds. These are coordination observations and rework signals,
not a measured productivity improvement.

## Shared test-resource queue

Gate runs, rehearsals, and landing candidates reserve Tower's per-repo
scratch resource. Add names for shared databases, build directories, or
other exclusive services in `tower.toml`:

```toml
[gates]
check = "cargo check"
test = "cargo test"
resources = ["integration-database"]
timeout_secs = 900
```

Matching resource names are exclusive across repositories using the same
`TOWER_HOME`. Requests are FIFO among jobs with overlapping resources;
unrelated resources can proceed independently. Repeating an active
request reuses its queue entry. A blocked operation returns a
`queued: true` receipt with its resource job; retry that operation after
earlier work completes. Scheduler iterations retry pending verification.
The queue does not run commands that bypass Tower.

```sh
tower resources list
tower resources cancel 12
# Only after checking the owner and all gate children have stopped:
tower resources recover 12 --confirmed-stopped
```

Cancelling a queued job removes it from the queue; cancelling a running
job asks its gate runner to stop. Gate execution has a timeout, defaults
to 900 seconds, and records the resource job ID alongside the existing
tree-bound gate proof. A crashed owner never loses its reservation just
because time passed. Recovery requires a dead owner and explicit
confirmation that its gate children stopped; it does not signal an
unverified PID. This queue coordinates one local Tower store, not
separate machines or independent `TOWER_HOME` directories.

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
exposing the original 20 tools plus `schedule`, `reconcile`, and
`resources` in v0.3.0 (23 total):
`board_get`, `task_create`,
`task_plan_save`, `claim_acquire`, `claim_release`, `worktree_create`,
`worktree_list`, `changeset_register`, `changeset_status`,
`changeset_refresh`, `changeset_supersede`, `changeset_conflicts`,
`integration_order`,
`gate_run`, `escalate_create`, `escalate_resolve`, `decision_list`,
`decision_log`, `events_since`, `report_create`, `schedule`, `reconcile`,
`resources`. `claim_acquire` returns the partial-grant
fields (`granted_nodes`, `refused`) and `claim_release` takes an optional
`nodes` array for per-node release. `changeset_status` includes
`claimed_vs_actual` (same shape as `board_get`), and
`changeset_conflicts` returns the read-only conflict detail above. Every
tool takes `repo` (path to a registered repo).

The three v0.3.0 tools take `action` and an `input` object. For example,
`schedule` with `action: "configure"` takes
`input: {"task_id": "T-2", "spec": {"depends_on": ["T-1"], "nodes": [], "max_attempts": 1}}`.
Use `action: "ready"` or `"status"` with an empty input to inspect;
`"tick"` takes `{"holder": "local-coordinator"}` and `"cancel"` takes
`{"task_id": "T-2"}`. `resources` provides `list`, `cancel`, and
`recover`; reconciliation actions are described above. The MCP server
does not start a coordinator loop on connection.

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
resources = ["integration-database"]  # optional exclusive resource names
timeout_secs = 900

[reserved]
paths = ["**/migrations/**", "build.rs", "**/*.lock"]
patterns = ["macro_rules!", "proc_macro"]

[escalation]
extra_triggers = []

[limits]
max_concurrent_assignments = 4
max_concurrent_reconciliations = 1

# Optional: configure this only to enable local agent dispatch.
[scheduler.agent]
program = "codex"
args = ["exec", "--json", "--sandbox", "workspace-write", "-"]
timeout_secs = 1800

[feedback]
repo = "Holy-Coders/tower"   # where Tower self-reports are filed
enabled = false              # local recording only; true opts into GitHub filing
```

The assignment limit applies to automatic dispatch; existing manual
assignment commands retain their behavior. `reserved` is parsed but not
enforced. Gates drive green/yellow/red beyond Tier-0.

## Local feedback and optional GitHub filing

Tower records bugs and inefficiencies in a local outbox. In v0.3.0,
remote filing is disabled by default and in newly generated configs.
Existing configs that explicitly set `feedback.enabled = true` remain
enabled. Opting in files reports as GitHub issues on the feedback repo
(default `Holy-Coders/tower`); use a suitable destination for workplace
projects.

**Explicit reports (agent-initiated).**

```sh
tower report --kind bug --title "board contradicts changeset status" \
  --body "board said green, status said red" --command "tower board"
tower report --kind inefficiency --title "gate run disrupted work" --body "..."
tower report list    # queued + filed, with states and occurrence counts
tower report flush   # try to file queued reports now
```

The MCP mirror is `report_create` (same fields plus `repo`); its tool
description tells agents to report Tower itself — unexpected failures,
contradictions between Tower commands or CLI/MCP, disruptive slowness,
workarounds — and never bugs in the code they are orchestrating. Each
report is enriched automatically with the Tower version, OS, timestamp,
kind, Tower IDs in context (recent task/assignment/changeset IDs), and
the triggering command when supplied.

**Automatic capture (Tower-initiated).** CLI dispatch and MCP tool
handling are wrapped: an internal Tower failure (internal error, a
panic caught at the dispatch boundary, an invariant contradiction)
drafts a `bug` report, and any operation over 5 seconds (except
`watch`, a loop by design, and the long-running MCP server itself)
drafts an `inefficiency` report with the measured duration. Expected
user errors (unknown IDs, unregistered repo, missing arguments,
refused claims, non-landable changesets) are not reported. Reporting
is a side effect that can never fail or change the outcome of the
operation that triggered it.

**Outbox, filing, dedupe, caps.** Reports persist in Tower's store
(signature, kind, title, body, context, state queued/filed, occurrence
count, first/last seen, filed URL). Filing uses the `gh` CLI
(`gh issue create --repo <feedback-repo> --label tower-auto-report
--label <kind>`; binary from PATH, overridable with `TOWER_GH`), with
a `<!-- tower-signature: ... -->` marker in the body. When enabled, filing is attempted
immediately after a report is created, at the start of `board` /
`changeset status` (CLI and MCP), and via `report flush` — always
best-effort and silent: if `gh` is missing, unauthenticated, or
offline, reports stay queued. The signature is a stable hash of the
kind, normalized title (lowercased, whitespace collapsed, varying
IDs/numbers stripped), and normalized command key; repeats coalesce
locally into one row with an occurrence count (crossing 10/100/1000
may add a comment to the filed issue). Before filing, open
`tower-auto-report` issues are searched for the signature and a match
is adopted instead of duplicated. At most 5 issues are filed per repo
per day (UTC); beyond the cap reports stay queued, never dropped.

**Configuration and opt-in.** In `tower.toml`:

```toml
[feedback]
repo = "Holy-Coders/tower"   # any "owner/name"
enabled = false              # set true only to opt into remote filing
```

**Scrubbing and review.** Reports contain the supplied title/body and
automatic context; Tower does not read source files or diffs to enrich
them. The home directory prefix is stripped from paths (repo-relative
where possible), and the value of any captured flag whose name contains
`key`, `token`, `secret`, `password`, or `auth` is replaced with
`[redacted]` before anything is stored or filed (unit-tested with a
planted fake secret). Arbitrary text in error messages or supplied report
bodies can still be confidential. Review what is suitable for the
configured destination before enabling remote filing.

## Verify your build

```sh
cargo test                # unit/integration tests, including scheduling, supervision and resource recovery
scripts/demo.sh           # 6-step acceptance demo, PASS/FAIL per assertion (51)
scripts/demo-phase21.sh   # sequential candidate landing + refused conflict (47)
scripts/demo-phase22.sh   # claims survive landing, per-test-fn claims, conflict hunks (60)
scripts/demo-phase23.sh   # self-reporting vs a stub gh: file, coalesce, queue, flush, cap (51)
```

[DOGFOOD.md](DOGFOOD.md) describes the bounded synthetic overlap exercise,
its isolation and receipts, and the separate handoff for any later live
pilot. Synthetic results do not establish a speed improvement on real work.

## Known limitations / stubs

- **Out of scope, stubbed**: `tower lsp`, `tower duplicates`, `tower github`
  print "not implemented in v1". No LSP adapters, duplicate detection,
  affected-test selection, GitHub adapter, or Windows support.
- **Watch** polls every 2s (no filesystem watcher in v1).
- **Integration order** is changeset creation order; there is no
  item-reference dependency graph yet. Declared task dependencies govern
  scheduling and are satisfied by landing the prerequisite task.
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
- **`[reserved]` and `[escalation] extra_triggers`** are parsed but not
  enforced; escalations are created explicitly via CLI/MCP. The assignment
  limit is enforced by the scheduler, not by manual assignment creation.
- **Local execution** uses an explicitly configured executable and Unix
  process groups. Tower does not provide hosted-agent APIs or grant
  credentials. Agent logs/prompts are local state and may contain task
  details; protect the configured `TOWER_HOME` accordingly.
- **Coordination is cooperative.** Agents must poll events and act on
  handoff instructions. Tower does not inject messages into existing
  sessions, rewrite their drafts, or infer undeclared dependencies.
- **Shared resources** are serialized only for commands routed through
  the same Tower store. Separate machines and commands outside Tower
  require their own coordination.
- **`cargo install --path .`** is not run by the demo or tests; the demo
  uses `target/release/tower` (override with `TOWER_BIN`).
- Radar simulation of *uncommitted* work uses a temporary git index to make
  a read-only snapshot commit object; user branches, index, and files are
  never modified (covered by the safety test).
