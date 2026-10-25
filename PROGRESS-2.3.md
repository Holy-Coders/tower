# Tower Phase 2.3 — Progress

## Status: COMPLETE (2026-10-25)

Source of requirements: the Phase 2.3 brief — self-reporting: agents
(and Tower itself) file GitHub issues on the Tower repo for Tower's
own bugs and inefficiencies, through a local outbox filed best-effort
via the `gh` CLI. Crate version bumped to **0.2.0**. Store changes are
additive (one new table, no changes to existing rows or to
claims/changeset/gate semantics).

Final verified numbers: `cargo build --release` clean and
**warning-free**, `cargo test` **45/45 green**,
`bash scripts/demo.sh` **51/51 PASS**,
`bash scripts/demo-phase21.sh` **47/47 PASS**,
`bash scripts/demo-phase22.sh` **60/60 PASS**, and
`bash scripts/demo-phase23.sh` **51/51 PASS** (all exit 0). Changes are
left uncommitted in the working tree.

Baseline (verified before edits): `cargo test` 37/37 green. The eight
new Phase 2.3 tests were written first and observed failing against
the Phase 2.2 code (unresolved `crate::feedback`, no
`TowerConfig.feedback` field, no `Store::list_reports`, MCP tool count
still 19), then made to pass.

## Stage log

- [x] **Store outbox** — new `reports` table, created additively in
  `migrate` (signature, kind, title, body, context JSON, state
  queued/filed, occurrence count, first/last seen, filed URL/at, last
  commented threshold; unique per repo+signature) with
  `upsert_report_at` (insert-or-coalesce), `list_reports` /
  `queued_reports`, `mark_report_filed_at`, `filed_since_count`, and
  `report_created` / `report_coalesced` / `report_filed` events.
- [x] **Feedback core** (`src/feedback.rs`, DECISIONS 25–28) —
  signature normalization (lowercase, whitespace collapsed, digit
  runs and long hex tokens → `#`) + FNV-1a signature; scrubbing
  (repo-relative paths, home prefix stripped, values of any
  key/token/secret/password/auth-named flag or assignment redacted,
  applied before storage); `[feedback]` config (default repo
  `Holy-Coders/tower`, `enabled` default true, partial sections keep
  field defaults); auto context (version, OS/arch, timestamp, kind,
  recent task/assignment/changeset IDs, scrubbed command); issue body
  with context block + `<!-- tower-signature: ... -->`; filing via
  `gh issue create` (`TOWER_GH` or PATH) with remote dedupe against
  open `tower-auto-report` issues, a 5-per-UTC-day cap, and
  10/100/1000-occurrence comments; every gh failure silent with the
  report left queued.
- [x] **Explicit surfaces** — CLI `tower report --kind bug|inefficiency
  --title ... --body ... [--command ...]` plus `report list`,
  `report flush`, and `report create`; MCP `report_create` (tools
  19 → 20) with the brief's instructive description (report Tower
  itself, never the orchestrated code). Flush on create and at the
  start of `board` / `changeset status`, CLI and MCP.
- [x] **Auto-capture** — CLI `main` and MCP `dispatch` wrap dispatch
  in `catch_unwind` + timing + error classification: panics draft a
  bug report and resume unchanged; `is_internal_error` drafts only
  Tower-fault failures (expected user errors excluded); operations
  over 5s draft an inefficiency (excluding `watch`, the `serve-mcp`
  process itself — its tool calls are timed instead — and `report`,
  so filing never recurses). `TOWER_SLOW_REPORT_MS` overrides the
  threshold for tests/demos. Capture never changes the outcome.
- [x] **Tests + demo** — 8 new tests (37 → 45): signature
  normalization/coalescing, scrubbing incl. the planted-fake-secret
  issue-body test, config defaults/parsing/partial sections,
  coalescing + daily-cap flush (5 of many queued), stub-gh
  integration (a) explicit file with labels/signature (b) coalesce
  with no second create (c) gh failure → queued, create still Ok
  (d) later flush files it, remote dedupe adopting an existing
  issue without creating, slow/internal classification + thresholds,
  and MCP `report_create` as tool 20 filing through `TOWER_GH`.
  Updated: MCP count 19 → 20 in `src/tests.rs`, `scripts/demo.sh`,
  `scripts/demo-phase22.sh`, `scripts/install.sh`. New
  `scripts/demo-phase23.sh` (51 assertions) runs (a)–(d) plus the
  `board` opportunistic flush, an automatic slow report,
  `enabled = false`, and the daily cap, all against stub `gh`
  scripts — no real issue is ever touched.
- [x] **Docs** — README "Self-reporting" section (what is captured,
  where issues go, `[feedback]` config/disabling, privacy rules) and
  MCP list at 20 tools; DECISIONS.md entries 25–28; this file;
  `reports/phase23-summary.md`.
- [x] **Verify** — exact commands below, all exit 0.

## Verification (exact)

```sh
cargo build --release   # Finished release, no warnings
cargo test              # 45 passed; 0 failed
bash scripts/demo.sh          # DEMO RESULT: 51 passed, 0 failed
bash scripts/demo-phase21.sh  # DEMO-PHASE21 RESULT: 47 passed, 0 failed
bash scripts/demo-phase22.sh  # DEMO-PHASE22 RESULT: 60 passed, 0 failed
bash scripts/demo-phase23.sh  # DEMO-PHASE23 RESULT: 51 passed, 0 failed
```

## Deferred / unchanged by design

- Filing goes only to the configured feedback repo, only through
  `gh`; there is no GitHub API client and no token handling in Tower.
  If `gh issue list --json` is unsupported, remote dedupe degrades
  to matching the signature in the plain listing (and then to filing
  un-deduped) — local coalescing and the daily cap still apply.
- The internal-error classifier is deliberately conservative: a
  Tower fault whose message carries none of the internal markers is
  not auto-drafted. Explicit agent reports remain the catch-all.
- Occurrence comments name the crossed round threshold (10/100/1000),
  not the exact running count at comment time.
- No web UI; `tower report list` is the inspection surface, and the
  daily cap's "later days" flush relies on the next create/flush/
  board/status that happens after UTC midnight — there is no daemon.
- Auto-captured slow reports include threshold-configurable timing
  (`TOWER_SLOW_REPORT_MS`); the default and documented threshold is
  5 seconds.
