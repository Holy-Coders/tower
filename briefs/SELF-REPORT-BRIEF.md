# Tower Phase 2.3 brief — self-reporting: agents file GitHub issues for Tower bugs and inefficiencies

## Why

Agents using Tower hit Tower's own bugs and inefficiencies (wrong output, contradictions between commands, slow operations, awkward surfaces) and today that signal is lost. Tower should capture it and open GitHub issues on the Tower repo automatically, so using Tower improves Tower.

Target repo for issues: `Holy-Coders/tower` (configurable, see below). This brief is a normal Tower phase: implement, test failing-first where practical, keep every existing demo green, log a DECISIONS.md entry, write PROGRESS-2.3.md and reports/phase23-summary.md. Breaking changes are acceptable (Tower is not in production use), but prefer additive store changes. Bump the crate version to 0.2.0.

## The feature

### 1. Explicit reports (agent-initiated)

- New CLI: `tower report --kind bug|inefficiency --title "<short>" --body "<details>"` with optional `--command "<the tower invocation that misbehaved>"`. Runs against the current repo context like other commands.
- New MCP tool `report_create` with the same fields (plus the usual `repo` argument). This makes the MCP surface 20 tools.
- The MCP tool's description must instruct agents when to use it, in substance: "Report Tower itself: a command failed unexpectedly, output contradicts Tower's own state (e.g. board vs changeset status), CLI and MCP disagree, an operation was slow enough to disrupt your work, or you had to work around Tower. Do NOT report bugs in the code you are orchestrating — only bugs or inefficiencies in Tower."
- A report is enriched automatically: tower version, OS, timestamp, kind, the repo's Tower IDs in context if cheaply available (current task/assignment/changeset IDs from state), and the triggering command if supplied.

### 2. Automatic capture (Tower-initiated)

- Wrap CLI command dispatch and MCP tool handling: if Tower itself errors internally (a failure that is Tower's fault — internal error, panic caught at the boundary, or an invariant contradiction Tower detects in its own output, e.g. claimed-vs-actual mismatch it cannot explain), draft a `bug` report automatically into the outbox with the error, command, and arguments (scrubbed, see §5).
- Slow operations: time each CLI command / MCP tool call. If one exceeds 5 seconds (excluding `watch`, which is a loop by design), draft an `inefficiency` report with the operation name and measured duration. Dedupe (§4) keeps this from spamming.
- Auto-capture must never change the outcome of the operation that triggered it: reporting is a side effect that cannot fail the user's command.

### 3. Outbox and filing

- Reports persist in Tower's store (new table is fine): fields at least signature, kind, title, body, context JSON, state (queued/filed), occurrence count, first/last seen, filed issue URL when known.
- Filing uses the `gh` CLI: `gh issue create --repo <feedback-repo> --title <title> --body <body> --label tower-auto-report --label <kind>`. The gh binary is resolved from PATH, overridable by env `TOWER_GH` (tests depend on this). Issue body = report body + the auto context block + a final line `<!-- tower-signature: <sig> -->`.
- Feedback repo: default `Holy-Coders/tower`; overridable in `tower.toml` under `[feedback]` with `repo = "owner/name"` and `enabled = true|false`. `enabled = false` means reports are recorded locally and never filed.
- Flush points: immediately after a report is created, and opportunistically at the start of `board` / `changeset status` (CLI and MCP). Flush is best-effort and silent on failure: if gh is missing, unauthenticated, or offline, reports stay queued and nothing the user asked for is blocked or failed.
- Also add `tower report list` (queued + recently filed, with states and occurrence counts) and `tower report flush` (manual flush attempt) so a human can inspect the pipeline.

### 4. Dedupe and caps

- Signature: a stable hash of (kind + normalized title + normalized error/command key). Normalization: lowercase, collapse whitespace, strip IDs/numbers that vary per occurrence.
- Local coalescing: a new report whose signature matches a queued or already-filed report increments its occurrence count and updates last-seen instead of creating a second entry. When a coalesced report's count crosses 10, 100, 1000, that fact may be appended as a comment on the filed issue via `gh issue comment` (best-effort).
- Remote dedupe: before filing, search the feedback repo's open issues for the signature marker (`gh issue list --repo <repo> --label tower-auto-report --state open --limit 100` and match the signature comment, or `gh issue list --search` if reliable in your testing). If found, mark the local report filed with that issue URL instead of opening a duplicate.
- Cap: at most 5 issues filed per repo per day. Beyond the cap, reports stay queued with counts (never dropped) and flush on later days.

### 5. Privacy and scrubbing (hard requirements)

- Never include file contents, diffs, code, secrets, tokens, or environment values in a report.
- Strip the user's home-directory prefix from any path; prefer repo-relative paths.
- In captured command arguments, redact the value of any flag whose name contains key, token, secret, password, or auth.
- Add unit tests proving a planted fake secret in a command string does not appear in the generated issue body.

### 6. Docs and install

- README: new section documenting self-reporting — what gets captured, where issues go, the `[feedback]` config, how to disable, the privacy rules.
- `scripts/install.sh`: the MCP smoke test currently asserts exactly 19 tools; update it to 20.
- DECISIONS.md: add the Phase 2.3 entries (number them continuing the existing sequence).

### 7. Tests and demo

- Unit tests: signature normalization/coalescing, scrubbing (incl. the fake-secret test), daily-cap logic, config parsing.
- Integration tests with a stub gh (a script injected via `TOWER_GH` that logs argv and prints a fake issue URL; a second mode that exits nonzero to simulate no-auth): (a) explicit report files an issue with the expected title/labels/signature; (b) same signature again coalesces and does not file a second issue; (c) gh failure leaves the report queued and the triggering command still exits successfully; (d) a later flush files the queued report.
- `scripts/demo-phase23.sh`: end-to-end demo of the above flows against the stub, in the style of the existing demo scripts, all checks passing.
- Regression bar: `cargo test` fully green; scripts/demo.sh, demo-phase21.sh, demo-phase22.sh still fully green; build with zero warnings.

## Out of scope

- Filing issues to any repo other than the configured feedback repo.
- Reading or including the orchestrated project's code in reports.
- Any change to claims/changeset/gate semantics.
- A web UI or dashboard; `report list` is the inspection surface.
