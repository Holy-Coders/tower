# Bounded coordination dogfood

The first exercise uses a synthetic repository and a deterministic local
Python worker. It checks Tower's coordination protocol, launch adapter,
gate queue, receipts, and consumer handoffs. It does not test an AI
agent's ability to infer or implement both requirements, and it does not
use provider APIs, production calls, or live project state. A later pilot
on real work is a separate step through the existing project coordinator.

## Executable and isolation

Use the v0.3.0 candidate binary from this checkout. Do not install it over
the binary serving existing work. The local build and exercise commands
are:

```sh
cargo build --locked --offline --jobs 1
./target/debug/tower --version
shasum -a 256 ./target/debug/tower
python3 scripts/demo-phase30.py --tower "$PWD/target/debug/tower" --output /tmp/tower-phase30-report.json
```

The version must report `tower 0.3.0`. Record the executable's absolute
path and hash alongside the report, source commit, and any uncommitted
release diff. A result from another binary is not evidence for this
candidate.

The script creates and retains one fixture root under
`/tmp/tower-phase30-*`, including its own repository, worktrees,
`TOWER_HOME`, local worker, logs, and receipts. Only the script's isolated
home is used for Tower state. It does not adopt a real repository, use the
running Tower home, replace the installed executable, change repository
visibility, or publish a release. Run this bounded fixture once and
inspect its result before considering a live pilot.

## Two intentions and one common contract

The shared implementation is `shared.py` with the authored interface:

```python
def settled_total(subtotal_cents: int, discount_cents: int) -> int:
    ...
```

| Participant | Intended change | Independent scope | Acceptance example |
| --- | --- | --- | --- |
| A | Clamp a discounted total below zero to zero. | `consumer_a.py` | `settled_total(100, 150) == 0` |
| B | Apply the discount to the subtotal. | `consumer_b.py` | `settled_total(100, 20) == 80` |

Both author their intent, assumptions, and executable acceptance commands.
The fixture assumes nonnegative, small integer cent values and checks
that a zero discount preserves the subtotal. These are synthetic
requirements, not claims about a production settlement policy.

The shared contract is explicit and versioned. A committed provisional
stub raises `NotImplementedError('TOWER_PROVISIONAL R-1')`; it must never
count as implemented behavior or become landable. Both participants pin
their original drafts, acknowledge a shared-scope pause, merge the exact
stub in their own fixture worktrees, and acknowledge its contract version.
`shared.py` uses a conservative whole-file claim because Python has no
item index in Tower. Rust and TypeScript item-scope behavior is covered
by separate tests.

## Bounded run and evidence

The fixture drives this sequence:

1. Create the two assignments, preserve each distinct draft, and let a
   coordinator tick discover one reconciliation from their recorded
   briefs and schedule specifications. Confirm both immutable draft
   commits, assumptions and acceptance commands are retained. Subsequent
   ticks must not create duplicate dispatches or a new handoff for an
   identical returned implementation.
2. Complete both cooperative handoffs and stub acknowledgements. Confirm
   dispatch waits for them and the unresolved stub cannot land. Ordinary
   assignment slots may be occupied; the designated reconciler has its
   own `max_concurrent_reconciliations = 1` allowance.
3. Dispatch the explicitly configured fake Python agent through the real
   local launch adapter. It waits on a fixture permit file while the
   original consumers make independent changes in their own files. Tick
   again to check that the same attempt does not launch twice.
4. Release the fixture permit. The fake worker writes and commits the
   predetermined combined implementation,
   `max(subtotal_cents - discount_cents, 0)`. Tower registers its result
   and runs the pure `verify.py` gate through the resource queue. Capture
   the resource job, current gate proof, returned commit, contract
   version/hash, and participant IDs.
5. Have both fixture consumers merge that exact returned commit,
   preserving their independent changes. Register each downstream
   changeset, rerun its behavioral gate, and acknowledge `resume`.
   Confirm both fresh green receipts and the final completed
   reconciliation, with no provisional stub accepted as final work.

The script prints JSON to stdout and can write the same report through
`--output`. It records the binary path/hash, isolated home and repository,
coordinator holder, reconciliation/task IDs, actual fake-worker launch
count, and durable attempt count. Keep its reported paths and receipts for review.
Assess successful handoffs, draft preservation, independent-work
preservation, launch count, gate/consumer validation, and any blocked or
uncertain outcome. Report observed stage durations, queue waiting, and
failure/retry counts when present. A bounded fixture without a real-work
baseline establishes no speedup, productivity gain, or semantic
reconciliation quality.

## Stop and rollback

If the fixture fails, retain its root and report. Stop only its bounded
script/coordinator and its identified fixture worker. If a worker is still
active, use the candidate binary with the fixture's exact `TOWER_HOME`
and repository path to request `schedule cancel` for the reported task;
observe until a terminal receipt confirms it stopped. Release only the
fixture coordinator's named lease. Do not clear uncertain reservations
or signal a stored PID on assumption.

For inspection, set these two paths from the retained report and the
candidate used for the run:

```sh
tower_candidate="$PWD/target/debug/tower"
tower_fixture="/tmp/tower-phase30-REPLACE-WITH-REPORTED-SUFFIX"
TOWER_HOME="$tower_fixture/state" "$tower_candidate" --repo "$tower_fixture/repo" schedule status
TOWER_HOME="$tower_fixture/state" "$tower_candidate" --repo "$tower_fixture/repo" resources list
```

Set `tower_fixture_task` to the report's `reconciliation_task_id` after
checking that status, then request cancellation only if it is active:

```sh
TOWER_HOME="$tower_fixture/state" "$tower_candidate" --repo "$tower_fixture/repo" schedule cancel "$tower_fixture_task"
```

Use the reported `run_dir` for its terminal `result.json` receipt. Once
all fixture execution has stopped, release its lease with the same
isolated paths:

```sh
TOWER_HOME="$tower_fixture/state" "$tower_candidate" --repo "$tower_fixture/repo" lease release --holder synthetic-pilot
```

No live binary or project state was changed, so rollback consists of
stopping fixture work and declining to install/adopt the candidate. Keep
the fixture evidence until all its processes are confirmed stopped and
the result has been reviewed; no live worktree cleanup is part of this
exercise.

## Handoff for a real-work pilot

Any later real-work pilot belongs to that project's existing
coordinator. This exercise does not independently create or modify its
tasks, lanes, leases, claims, running processes, or provider configuration.
Hand that coordinator the candidate executable/version/hash, source
revision/diff, synthetic report and retained evidence paths, known
limitations, and an explicit stop/rollback point. Keep private project
targets, draft/base hashes and acceptance manifests in the project's
local handoff rather than this public candidate.

Before dispatch, the coordinator must verify the exact authorized drafts
and base, capture both workers' intent and executable acceptance criteria,
agree on the preserved interface, arrange cooperative handoffs, and
establish how candidate state shares coordination ownership with its
existing workflow. Hold the pilot on stale or failed proof, uncertain
ownership, or unqualified required behavior. Both original workers must
validate the exact returned result before the handoff completes. A
synthetic pass does not qualify real flows. The live pilot and its
rollback remain pending that separate coordinator's acceptance.
