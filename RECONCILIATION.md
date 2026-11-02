# Reconcile shared edits without losing either draft

Tower can coordinate a third worker when two assignments need the same function
or file. A reconciliation is a durable `R-` record and an ordinary scheduled
task. Both original requirements and pinned git snapshots become the third
worker's input. Tower validates its committed result and returns a commit receipt
to the original assignments through the board and event feed.

Workers must cooperate with the handoff. Tower does not interrupt an arbitrary
editor, invent a common interface, or edit an original worker's files, index or
branch. A text merge alone cannot establish that both requirements are satisfied.
Configure behavioral gates that exercise both requirements, and review the
result. The local configured agent adapter supplies the third worker; existing
workers consume Tower events and acknowledge their own clean commits.

## Workflow

`tower reconcile help` prints the exact JSON input examples. The CLI reads an
action's JSON from `--input PATH`; the `reconcile` MCP tool accepts the same action
and input. For example:

```json
{
  "participants": [
    {"assignment_id":"A-1", "intent":"Apply the required rounding rule to shared totals.",
     "assumptions":["total returns integer cents"],
     "acceptance_tests":["cargo test rounding_rule"]},
    {"assignment_id":"A-2", "intent":"Include the discount before calculating the shared total.",
     "assumptions":["discount is applied before tax"],
     "acceptance_tests":["cargo test discount_before_tax"]}
  ],
  "nodes": [{"file":"src/totals.rs", "item":"total"}]
}
```

1. Run `tower reconcile discover` to inspect structural overlap. Rust and
   TypeScript use indexed items; unknown or ambiguous scope uses the whole file.
   Identical shared implementations do not trigger repeated reconciliation.
   Submit both authored requirements with `tower reconcile request --input ...`.
   This pins both drafts under `refs/tower/reconciliations/R-*/draft-*`, including
   uncommitted draft snapshots without touching either working tree. The request
   automatically creates a scheduled reconciliation task, using the configured
   agent when ready. Repeating identical input returns the same reconciliation.
   Both participants must supply explicit acceptance commands before dispatch.
   Automatically discovered needs without those commands remain pending. Use
   `intent` to supply or amend a goal, interface assumptions and acceptance
   commands before dispatch; doing so invalidates the changed worker's pause
   and both provisional acknowledgements. Tower never fabricates the criteria.
2. Each original worker stops edits to the overlapping scope, commits its draft,
   and calls `pause` with its assignment and exact clean `HEAD`. The captured draft
   must be preserved by tree identity or ancestry. Other claimed files and items
   remain owned by their original workers. Shared-scope reservations survive
   process restarts and cannot be evaded by releasing an ordinary claim.
3. Optionally, a worker supplies an explicitly authored common interface and a
   committed stub with `contract`. Tower versions and hashes that text and pins
   the stub commit. Each consumer merges the stub while preserving its original
   draft, then calls `stub-ack` with the exact clean commit and contract version.
   A contract update invalidates both acknowledgements. The stub is unresolved:
   independent work can continue, but none of the three shared changes can land.
   Without a stable contract, work depending on the overlap stays paused.
4. The coordinator dispatches when both handoffs (and any stub acknowledgements)
   are ready. Ownership transfers atomically for the shared scope only. An
   existing whole-file claim conservatively widens the handoff to the whole file;
   Tower never silently drops the rest of a broad claim. The reconciliation
   worker receives both intents, immutable draft commits, common base and any
   contract. It implements only that scope, commits, and runs configured gates.
   Inputs remain pinned even if the third worker fails.
5. The coordinator publishes a returned-result receipt after current, proven
   gates and both workers' explicit acceptance commands pass. `publish` is also
   available explicitly. Acceptance commands execute in the reconciler's exact
   clean committed worktree under the configured shared test-resource queue,
   with bounded execution and cancellation. Receipts bind command, HEAD, tree,
   contract version, result and duration. A command that changes tracked or
   untracked worktree state cannot prove that unchanged commit. The result must descend
   the common base and provisional stub when supplied, stay inside the shared
   scope, and change the provisional implementation. A legacy unbound gate pass
   cannot prove reconciliation. Original workers may continue independent work,
   but changing a frozen shared function invalidates the handoff and requires
   cancellation and renegotiation.
6. Each original worker merges the returned result, resolves its own downstream
   integration, registers a changeset, runs its configured gates again, and calls
   `resume` with that current green changeset. Its head must contain both its
   paused draft and the exact returned result. Virtual validation coverage applies
   only to the exact returned shared implementation; it does not grant a second
   writer ownership. `resume` reruns that consumer's explicit acceptance commands
   against its exact clean downstream commit. Both consumers' heads and proofs are checked again before
   the reconciliation completes. Until then, landing is blocked for both
   originals and the reconciler. Completion releases the reconciler's shared
   claims. Its assignment remains bound to that exact immutable returned commit;
   original assignments may continue independent work while retaining the exact
   verified shared implementation. Further shared edits use a new task and
   ownership handoff, so completion cannot authorize reintroducing an old stub.

Original draft changesets stay visible with a shared-reconciliation hint while
they are suspended from integration order, radar pairing and gate rehearsals.
They rejoin when they contain the exact returned implementation. Unrelated
assignments continue normally.

## Measuring a pilot

`tower reconcile metrics --input ...` returns the state timeline, request-to-ready
wait, ready-to-dispatch wait, dispatch-to-return time and return-to-both-consumers
validated time. It includes contract-version count, failed acceptance checks,
durations and commit-bound receipts, plus any actual landing events. State events
have whole-second resolution. Missing phases remain null. These measurements
support comparing a bounded pilot; Tower does not claim a productivity improvement
from one successful merge or from these counters alone.

## Cancellation and recovery

`cancel` records a reason. Before dispatch, cancellation releases the reservation
when no provisional contract was supplied. After ownership transfer or a
contract, cancellation enters `cancelled_held`: reservations remain held because
neither process termination nor stub removal can be assumed. Cancel the scheduled
agent separately; its durable adapter receipt must establish a terminal state.

To recover, both original workers restore the pre-stub shared implementation in
new commits that preserve their original drafts, then submit `recovery-ack` with
their exact clean heads. `recover` rechecks both heads and every reconciliation
attempt's terminal receipt against its expected worktree. Missing or uncertain
receipts keep the reservation held. An untracked reconciliation assignment must
be closed. Recovery restores only the actual original claim holders; it never
grants shared ownership to both workers. Pinned drafts and results are retained.

An uncertain adapter result needs inspection; Tower does not launch a replacement
worker or clear reservations merely because a heartbeat expired. A cancelled run
that never launched and has no executable request also fails the receipt check;
it requires inspection rather than an assumption that a worker stopped.

## Limits of the protocol

The intent is supplied by the two original assignments. Structural overlap is
conservative, and tests plus human review establish behavior. Agents must read
events and perform their own merges and acknowledgements; Tower cannot resume an
arbitrary agent through an unsupported provider control API. Contracts must be
authored explicitly and represented by git commits. The generic protocol can
reject an unchanged stub, stale proof, stale handoff or unacknowledged consumer;
it cannot determine whether a changed implementation satisfies arbitrary prose.

All fixtures use temporary repositories and Tower homes. This feature does not
upgrade the running installed binary or adopt existing live coordination state.
