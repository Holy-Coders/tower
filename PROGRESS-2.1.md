# Tower Phase 2.1 — Progress

## Status: COMPLETE (2026-10-25)

Source of requirements: `~/workspace/tower-playtest-long/REPORT.md` (F1–F6)
plus the Phase 2.1 brief (six fixes + board next-action).

Final verified numbers: `cargo build --release` clean and **warning-free**,
`cargo test` **30/30 green**, `bash scripts/demo.sh` **51/51 PASS**,
`bash scripts/demo-phase21.sh` **47/47 PASS** (all exit 0). Changes are
left uncommitted in the working tree, per the brief.

Baseline (verified before edits): `cargo build --release` clean,
`cargo test` 17/17 green.

## Stage log

- [x] **S1 Claims (F3)** — partial grants (`granted`/`granted_nodes`/`refused`,
  CLI exit 1 only on zero granted) and per-node `claim release --node`
  (CLI + MCP `claim_release.nodes`). Store: `acquire_claims_partial`,
  `release_claims_nodes`; old `acquire_claims` kept as a wrapper.
- [x] **S2 Additive migrations** — `changesets.version/superseded/superseded_by`,
  `gate_runs.tree_hash/main_head/participants_json`, added via
  `table_info` checks. Verified against a hand-built v1-schema DB in tests
  and against a *copy* of the real playtest home (7 changesets migrate;
  the original home was not touched).
- [x] **S3 Status engine (F1+F6+F2-live)** — order-aware conflicts (later =
  informational `blocks …`), gate-proof staleness (participant snapshots +
  recorded main head must still match; legacy proof-less rows
  grandfathered), live claimed-vs-actual recomputed from live claims and
  the live diff on every status refresh. Snapshot commits are now
  date-deterministic so proofs compare equal for unchanged dirty trees.
- [x] **S4 Refresh/supersede (F2+F4)** — `changeset refresh`/`supersede`
  (CLI + MCP `changeset_refresh`/`changeset_supersede`), re-register
  supersedes same-assignment unlanded, landing supersedes same-task
  siblings; superseded excluded from order, status refresh, radar pairing,
  gate attribution; board gains a SUPERSEDED section.
- [x] **S5 Candidate landing (F5)** — Tower-owned landing scratch (separate
  from rehearsal), merge of the committed head onto current main, gates
  on the candidate tree with proof fields, moved-head rebuild-once,
  CAS/`--ff-only` advance, landed event with source/candidate/based-on
  heads. Landable = green, or yellow whose only blockers are gate proofs.
  Green fast-forward mechanics unchanged.
- [x] **S6 Board next-action** — pure `next` hint in text, JSON, and HTML
  boards and in `changeset status`.
- [x] **S7 Tests** — 13 new tests (17 → 30): order-aware green/red,
  candidate landing (checked-out + CAS main), candidate conflict,
  candidate gate failure, stale proof (peer + main moves), refresh +
  re-register supersede, landing-supersedes-siblings, partial claims,
  per-node release, next-action mapping, v1-home migration, superseded
  radar/gate exclusion. Intentional-behavior updates: the order-blind red
  assertions, "yellow can never land", and the MCP tool count (16 → 18),
  in both `src/tests.rs` and `scripts/demo.sh` (still 51 assertions).
- [x] **S8 Demo** — `scripts/demo-phase21.sh` (47 assertions): two
  compatible non-fast-forward changesets landed sequentially via
  candidates with worker worktrees byte-untouched, one conflicting
  changeset refused with named files, plus partial claims/per-node
  release/next-action checks.
- [x] **S9 Docs** — README (candidate landing, order-aware statuses, gate
  staleness, refresh/supersede, partial claims, next-actions, limitations),
  DECISIONS.md entries 14–20, this file, `reports/phase21-summary.md`.
- [x] **S10 Verify** — exact commands below, all exit 0.

## Verification (exact)

```sh
export PATH="$HOME/.cargo/bin:$PATH"
cargo build --release   # Finished release, no warnings
cargo test              # 30 passed; 0 failed
bash scripts/demo.sh          # DEMO RESULT: 51 passed, 0 failed
bash scripts/demo-phase21.sh  # DEMO-PHASE21 RESULT: 47 passed, 0 failed
```

## Deferred / unchanged by design

- No conflict *resolution* inside a candidate (refuse-with-files only).
- Rebuild-once on moved heads is race logic; it is covered by construction
  and the CAS/`--ff-only` expected-head checks, not by a concurrency test.
- Everything listed as deferred in `PROGRESS.md` (watch polling, creation
  integration order, unenforced config sections) is unchanged.
