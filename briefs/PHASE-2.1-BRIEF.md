# Tower Phase 2.1 — fix the six playtest findings

You are working in the Tower repo (this directory): a Rust CLI+MCP tool for
orchestrating parallel agent work in git worktrees. Phases 1+2 are complete
and verified (see PROGRESS.md, DECISIONS.md, README.md). A long playtest
(3 tasks × the same 3 files × 3 change-request rounds) then exposed six
concrete defects. Read `~/workspace/tower-playtest-long/REPORT.md` first —
it is the requirements source. This brief turns its findings F1–F6 plus one
board improvement into implementation work.

First step (checkable artifact): create `PROGRESS-2.1.md` with your plan,
then keep it updated as stages complete.

## Fixes to implement

### 1. Order-aware statuses (F1)
`refresh_changeset_statuses` currently treats merge conflicts
symmetrically, so the first changeset in integration order is red merely
because later changesets conflict with it, and a conflicting set deadlocks.
Change the semantics:
- A changeset is blocked (red) by conflicts with the main branch and with
  *earlier* unlanded, non-superseded changesets in integration order.
- Conflicts with *later* changesets do not redden it; record them as an
  informational reason (e.g. "blocks C-3 in src/x.rs" style wording of
  your choice) that does not affect its status.
- The later changeset keeps the blocking reason naming the earlier one.

### 2. Tower-owned landing candidates (F5)
Extend `land_changeset`:
- Fast-forward case unchanged.
- If the changeset is landable but its branch does not fast-forward main,
  build a *landing candidate* in a Tower-owned scratch worktree (separate
  from the rehearsal scratch; never the worker's worktree): reset to the
  current main head, merge the changeset's committed branch head.
  - Merge conflict → abort the merge, mark the changeset red with the
    conflicted files, fail with a clear message. Worker worktree untouched.
  - Run the configured gates (Tier 1, Tier 2, whichever are configured)
    on the candidate tree; record them (see fix 3 for the proof fields).
    Gate failure → red, fail with the gate output tail.
  - Before advancing main, re-read the main head and the source branch
    head. If either moved since the candidate was built, rebuild the
    candidate once; if it moved again, fail asking for a retry.
  - Advance main only to the candidate head (it descends from the old
    main, so this is a fast-forward of the main ref; use the checked-out
    main worktree `merge --ff-only` when main is checked out and clean,
    otherwise `update-ref` with the expected old value as a
    compare-and-swap).
  - Emit the landed event with source head, candidate head, and the main
    head it was based on.
- Landability: accept status green, or yellow whose *only* blockers are
  pending/stale gate proofs (the candidate runs gates itself). Refuse red,
  and refuse yellow for other reasons (outside claim, etc.), with the
  reasons in the error.
- Keep all existing refusals: non-green/non-landable, dirty source
  worktree, dirty main worktree, uncommitted work (land committed heads
  only).

### 3. Gate verdicts bound to their proof context (F6)
A gate pass must stop counting when the tree it proved changes.
- Add columns to `gate_runs` (additive migration): `tree_hash` (the
  rehearsal/candidate tree hash, `HEAD^{tree}` of the scratch at gate
  time), `main_head`, and `participants_json` (map of changeset id →
  snapshot commit that went into that tree; for a candidate, just the
  landed changeset → its head).
- A recorded pass counts in status computation only if it is still
  current: the changeset's current snapshot equals the recorded
  participant snapshot for it, every other recorded participant's current
  snapshot is unchanged, and the recorded `main_head` equals the current
  main head. (For Tier-0-only repos there are no gates; unaffected.)
- When the latest pass is stale, the changeset is yellow with a reason
  like "gate proof stale (source, peer, or main changed) — rerun gates".

### 4. Changeset refresh, versioning, supersede (F2 + F4)
- Status refresh must recompute claimed-vs-actual from *live* claims and
  the *live* worktree diff (files/items), not the registration-time
  snapshot, so expanding claims (or narrowing the diff) can clear an
  outside-claim yellow without re-registering. Update the stored
  files/items/CVA when they change.
- Add `tower changeset refresh <id>` (CLI and MCP tool
  `changeset_refresh`) that forces this recomputation, bumps a `version`
  counter on the changeset row (add the column), records the new head, and
  emits a `changeset_refreshed` event.
- Supersede: add a `superseded` state. Re-registering an assignment
  supersedes its previous unlanded changesets. Landing a changeset
  supersedes other unlanded changesets of the same task. Add
  `tower changeset supersede <id>` (CLI and MCP `changeset_supersede`)
  for manual use. Superseded changesets are excluded from integration
  order, status refresh, radar pairing, and gate attribution; the board
  lists them in a separate short section (id + superseded-by if known)
  instead of the main changeset list, and they no longer produce phantom
  "conflicts with main" lines. Emit `changeset_superseded` events.

### 5. Partial claim grants and per-node release (F3)
- `claim acquire` grants the uncontested subset and reports the rest:
  response JSON keeps a top-level `granted` boolean (true only if all
  requested nodes were granted) and adds `granted_nodes` and `refused`
  (each with file, item, holding assignment). CLI exit code: 1 only when
  zero nodes were granted; 0 when at least one was granted.
- `claim release` accepts optional repeated `--node file#item`: with
  nodes, release only those; without, release all of the assignment's
  claims (current behavior). MCP `claim_release` mirrors this (optional
  `nodes` array).

### 6. Board next-action
Each changeset line on the board (text + JSON + HTML if trivially shared)
gets a `next` hint derived from its state: e.g. "land", "run gates",
"rerun gates (proof stale)", "resolve conflict with C-2 in src/x.rs",
"refresh claims or changeset", "wait: blocked by C-1", "superseded",
"landed". Keep it a pure function of the computed status/reasons.

## Constraints
- Do not commit or push; leave changes uncommitted in the working tree.
- Additive DB migrations only; existing Tower homes (SQLite DBs created by
  v1) must keep working.
- Keep every existing CLI command and MCP tool working. New surface is
  additive, except the claim response JSON gains fields and `granted`
  semantics stay as specified above.
- Update tests where behavior intentionally changed; do not weaken a test
  to hide a regression. All pre-existing tests must pass; `scripts/demo.sh`
  must still pass 51/51 (adjust only if a step's asserted behavior is one
  of the intentional changes above, and note it in DECISIONS.md).
- Cargo is at `~/.cargo/bin` (export PATH). Build must end warning-free.

## Required new tests / demo
Add tests covering, at minimum:
- order-aware status: first of a conflicting pair is landable-green after
  gates while the second is red naming the first;
- candidate landing: a green, non-fast-forward changeset lands via a
  candidate; the worker worktree's HEAD and files are untouched; main
  contains the change; landed event records source and candidate heads;
- candidate conflict: landing attempt fails, changeset red with files,
  main unchanged;
- stale gate proof: after a peer changeset's snapshot changes (or main
  moves), a previously green changeset drops to yellow with the stale
  reason until gates rerun;
- changeset refresh: expanding claims then `changeset refresh` clears an
  outside-claim yellow without re-registering; re-register supersedes the
  old record and the board/order exclude it;
- landing supersedes sibling changesets of the same task;
- partial claim grant: mixed bundle grants the free nodes, refuses the
  held ones by name, exit 0; a fully refused bundle exits 1;
- per-node claim release frees exactly one node.
Add `scripts/demo-phase21.sh` in the style of demo.sh: two changesets
forked from the same base with compatible-but-not-fast-forward changes
plus one conflicting changeset; land the first two sequentially via
candidates with no manual merges in worker worktrees; the conflicting one
is refused with named files; print PASS/FAIL per assertion and a summary.

## Docs
- README: document candidate landing, order-aware statuses, gate proof
  staleness, changeset refresh/supersede, partial claims, per-node
  release, board next-actions; update the limitations list (land is no
  longer "fast-forward only" — it is "fast-forward or Tower-built
  candidate").
- DECISIONS.md: add Phase 2.1 entries for each design choice you make.
- PROGRESS-2.1.md: stages and final verified numbers.
- reports/phase21-summary.md: what changed, test/demo counts, anything
  left stubbed or deferred, and Aaron's first commands to try it.

## Verification (run all, report exact results)
```
export PATH="$HOME/.cargo/bin:$PATH"
cargo build --release
cargo test
bash scripts/demo.sh
bash scripts/demo-phase21.sh
```
