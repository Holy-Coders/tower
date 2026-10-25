# Phase 2.3 summary — self-reporting: Tower files issues about itself

Tower now captures its own bugs and inefficiencies — reported
explicitly by agents, or automatically on internal failures and slow
operations — into a local outbox and files them best-effort as GitHub
issues on the feedback repo (default `Holy-Coders/tower`) via `gh`.
Changes are uncommitted in the working tree. Verified:
`cargo build --release` warning-free, `cargo test` **45/45**,
`scripts/demo.sh` **51/51**, `scripts/demo-phase21.sh` **47/47**,
`scripts/demo-phase22.sh` **60/60**, `scripts/demo-phase23.sh`
**51/51** (all exit 0). Crate version **0.2.0**. Design rationale is
in `DECISIONS.md` entries 25–28.

## What changed

- **Explicit reports** (`src/main.rs`, `src/mcp.rs`): CLI
  `tower report --kind bug|inefficiency --title ... --body ...
  [--command ...]`, plus `tower report list` (queued + filed, states,
  occurrence counts) and `tower report flush`; MCP `report_create`
  takes the MCP surface to **20 tools**, with a description that
  instructs agents to report Tower itself (unexpected failures,
  contradictions such as board vs changeset status, CLI/MCP
  disagreement, disruptive slowness, workarounds) and explicitly not
  bugs in the orchestrated code. Reports auto-enrich with Tower
  version, OS/arch, timestamp, kind, recent task/assignment/changeset
  IDs from state, and the triggering command.
- **Automatic capture** (`src/main.rs` CLI wrapper, `src/mcp.rs`
  `dispatch` wrapper): dispatch runs under `catch_unwind` + timing.
  A panic caught at the boundary drafts a bug report and resumes; an
  error drafts one only when `feedback::is_internal_error` classifies
  it as Tower's fault (unknown IDs, unregistered repos, missing args,
  refused claims, held leases, non-landable changesets are user
  errors and never reported); an operation over 5s drafts an
  inefficiency with the measured duration, excluding `watch`, the
  long-running `serve-mcp` process (its tool calls are timed
  individually), and `report` itself. The threshold is overridable
  with `TOWER_SLOW_REPORT_MS` for tests. Auto-capture discards its
  own result everywhere, so it cannot change a command's outcome.
- **Outbox and filing** (`src/feedback.rs`, `src/store.rs`,
  `src/config.rs`): an additive `reports` table holds signature,
  kind, title, body, context JSON, state, occurrence count,
  first/last seen, and the filed URL. Filing is `gh issue create
  --repo <feedback-repo> --label tower-auto-report --label <kind>`
  with a `<!-- tower-signature: ... -->` trailer; the binary is
  `TOWER_GH` or `gh` on PATH. Flush points are report creation, the
  start of `board` / `changeset status` (CLI and MCP), and `report
  flush` — always silent and best-effort, so a missing or
  unauthenticated `gh` (the `/usr/bin/gh` on this machine is not
  logged in) simply leaves reports queued. `[feedback] repo / enabled`
  configures the target; `enabled = false` records locally only.
- **Dedupe and caps**: the signature hashes kind + normalized title
  + normalized command key (lowercased, whitespace collapsed, digit
  runs and long hex tokens stripped to `#`). Repeats coalesce into
  the queued or filed row with an occurrence count, commenting on
  the filed issue when 10/100/1000 is crossed. Before creating, open
  labelled issues are listed (plain form per the brief, plus a
  `--json url,title,body` form whose bodies actually contain the
  signature marker) and a match is adopted instead of duplicated.
  Filing is capped at 5 issues per repo per UTC day; the rest stay
  queued, never dropped.
- **Privacy**: scrubbing runs before anything is stored — paths go
  repo-relative with the home prefix stripped, and the value of any
  flag, assignment, or `name: value` pair whose name contains
  key/token/secret/password/auth (substring, so `--api-key`, `API_KEY=`,
  spelled-out names) becomes `[redacted]`. Context carries no file
  contents, diffs, code, or environment values.

## Tests / demos

- `cargo test`: 45 tests (8 new, written first and observed failing
  on the Phase 2.2 tree as compile errors + the 19→20 count):
  signature normalization/coalescing; scrubbing incl. a planted fake
  secret asserted absent from both scrubbed output and the generated
  issue body; `[feedback]` defaults and partial-section parsing;
  coalescing plus a daily-cap flush that files exactly 5 of many
  queued; the four stub-`gh` integration flows (explicit file with
  repo/title/labels/signature, no second create on coalesce, failing
  `gh` → queued with creation still succeeding, later flush files
  it); remote dedupe adopting an existing issue URL without a
  create; slow/internal classification and occurrence thresholds;
  `report_create` as tool 20 filing through `TOWER_GH` with the
  planted token absent from the stub's argv log.
- `scripts/demo-phase23.sh` (new, 51/51): scratch repo + ok/failing
  stub `gh`. Explicit CLI report files with scrubbed secrets,
  coalesces on a case/ID variant, MCP lists 20 tools and
  `report_create` files, failing `gh` queues while exiting 0, flush
  and then an opportunistic `board` flush file the queue, a 1ms
  threshold drafts an automatic slow-board inefficiency,
  `enabled = false` records without filing, and re-enabling flushes
  only up to the 5-per-day cap with the rest still queued.
- Regressions unchanged except the 19→20 tool count (in
  `scripts/demo.sh`, `scripts/demo-phase22.sh`, `scripts/install.sh`,
  and one test): demo 51/51, demo-phase21 47/47, demo-phase22 60/60.

## Left stubbed / deferred

Nothing in the Phase 2.3 scope is stubbed. By design: the internal
classifier is conservative (a Tower fault with no internal marker in
its message is left to explicit agent reports); remote dedupe falls
back to un-deduped filing when `gh issue list --json` bodies are
unavailable (local coalescing + cap still bound the noise);
occurrence comments name the crossed threshold, not an exact count;
and "flush on later days" means the next create/flush/board/status
after UTC midnight — there is no background daemon. Reports are
stored per repo in Tower's home, like all other Tower state, so a
target repo never gains files beyond an optional `[feedback]`
section in `tower.toml` (added to the generated default config).

## Aaron's first commands

```sh
export PATH="$HOME/.cargo/bin:$PATH"
cd ~/workspace/code/holy-coders/tower
cargo build --release
cargo test                       # 45/45
bash scripts/demo-phase23.sh     # 51/51 against a stub gh — no real issues filed

# Against any registered repo (uses your real gh login when present):
./target/release/tower --repo <repo> report --kind inefficiency \
  --title "board felt slow" --body "details" --command "tower board"
./target/release/tower --repo <repo> report list   # queued/filed + counts
./target/release/tower --repo <repo> report flush  # retry the outbox now
# Quiet mode: set [feedback] enabled = false in <repo>/tower.toml
```
