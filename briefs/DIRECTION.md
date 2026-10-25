# Direction from Dobby (driver/reviewer)

This file is how Dobby directs the build. The builder (Muse Code) reads it at the start of every run and at every stage boundary. Dobby updates it as he monitors. Aaron is asleep and has delegated overnight decisions to Dobby (2026-10-25 ~01:30): Dobby's decisions below are Aaron's for tonight.

## Standing direction

- GO: all stages, in the order given in BUILD-BRIEF.md's Process section. You do not need to stop between stages; run the build through, keeping the logging protocol in the brief (PROGRESS.md at each stage boundary, reports/stage-<n>-report.md per stage with raw test output).
- Stage 0 (the build plan in reports/stage-0-plan.md) is part of the run: write the plan, then continue into Stage 1 in the same run.
- If this file gains a STOP or a Corrections section at any point, it overrides the standing GO: apply it at the next stage boundary before doing new work.
- Only Aaron in person can override a STOP from Dobby.

## Decisions pre-made by Dobby (do not stall on these)

1. MCP implementation: try `rmcp` briefly; if it costs real friction (over ~30 minutes), hand-roll the JSON-RPC subset (initialize, tools/list, tools/call) and note it in DECISIONS.md. The spec Section 11 tool list is the contract.
2. Demo program: write it in Rust (cargo) so the demo has no Node/npm dependency. Demo assertions unchanged.
3. Tree-sitter grammars: Rust and TypeScript are required. Add JavaScript only if it comes free with what you already have; do not block on more languages.
4. Watch loop: 2-second polling is acceptable for v1. Do not spend time on notify/FSEvents.
5. Board HTML: one static file regenerated when state changes; keep it simple. The CLI board is primary. No web server in v1 beyond the MCP stdio server.
6. State: one SQLite database at ~/.tower/tower.db for all registered repos, rows keyed by canonical repo path.
7. Timing-flaky tests: inject the poll interval / use tempdirs and explicit synchronization in tests instead of longer sleeps, and never delete a failing test to make the suite pass. If a test is genuinely wrong for the design, change it and justify the change in the stage report and DECISIONS.md.
8. `tower init` on test/demo repos only, using the --write-config flag. Never register or init any of Aaron's real repos tonight.

## Final stage additions (so it is ready for Aaron in the morning)

- Run `cargo install --path .` so `tower` lands on PATH (~/.cargo/bin/tower), then verify `tower --version` works and record the output in the final report.
- Run the full acceptance demo one final time **using the installed binary** and record its PASS/FAIL lines.
- Do NOT start `tower watch` as a persistent service and do NOT register any real repo. Leave both for Aaron.
- Write `reports/final-summary.md`: what was built, build/test/demo numbers (real), what was stubbed, install status and binary path, any DECISIONS.md entries that matter, and the exact first commands for Aaron (tower init / tower board / MCP snippet from the README).

## Corrections

(None yet.)

## Review log

- 2026-10-25 ~01:30 IDT — Dobby: overnight GO issued with pre-made decisions and final-stage additions above. Aaron asleep; Dobby monitoring via stage reports and will file corrections here if a review finds problems.
