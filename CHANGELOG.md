# Changelog

## 0.3.0 — 2026-11-02

- Add opt-in task dependency specifications, readiness reasons, and a
  coordinator loop that respects existing assignments, claims, the
  orchestrator lease, and the configured assignment limit.
- Reserve a separate reconciliation allowance (default 1) so participants
  waiting on shared work cannot starve their designated reconciler.
- Launch an explicitly configured local agent executable in each assigned
  worktree. Persist attempt reservations, supervisor heartbeats, logs,
  completion receipts, bounded retries, and cancellation requests;
  uncertain attempts retain ownership instead of launching a duplicate.
- Register committed agent results and run configured gates before
  presenting them for review. Landing remains an explicit operation.
- Add durable FIFO reservations around Tower scratch operations and
  configured shared test resources, with bounded gate execution,
  cancellation, deliberate crash recovery, and resource IDs in gate
  receipts.
- Add cooperative reconciliation records and handoffs for shared
  implementations, including authored assumptions and acceptance tests;
  see the README for the protocol and its limits.
- Add CLI and MCP surfaces for scheduling, reconciliation, and resources.
- Disable remote feedback filing by default and in newly generated
  configuration. Existing explicit opt-ins remain enabled.
- License Tower under MIT for reuse, including commercial and workplace use.
- Make the acceptance demos portable across BSD and GNU hosts; preserve
  board labels when worktree paths use a symlink alias.

## 0.2.0

- Add local self-report outbox, deduplication, privacy scrubbing, and
  best-effort filing through the GitHub CLI.
- Add the `report` CLI and `report_create` MCP tool.

## 0.1.0

- Initial local radar, task and assignment board, worktree claims,
  merge simulation, rehearsal gates, landing candidates, and MCP tools.
