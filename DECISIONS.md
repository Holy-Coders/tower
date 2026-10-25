# Decisions

Simpler interpretations chosen where the spec was ambiguous, per the build
brief. Each entry notes the alternative that was not taken.

1. **MCP server is hand-rolled** (stdio, JSON-RPC 2.0 line-delimited:
   `initialize`, `ping`, `tools/list`, `tools/call`) instead of the `rmcp`
   crate. The spec Section 11 tool list is the contract; the subset is
   small and dependency-free. `resources/read` is not implemented (optional
   per the brief).

2. **Watch polls every 2s** instead of using a filesystem watcher
   (`notify`). The brief allows this if documented; `tower watch` says so on
   startup. A collision is visible within one poll interval plus scan time.

3. **`tower land` is fast-forward only.** The spec says landing a green
   changeset should be "a formality (merge or PR, per repo config)". v1
   fast-forwards the main worktree (or `refs/heads/<main>` if main is not
   checked out), and refuses non-fast-forwards, dirty source/main
   worktrees, and non-green changesets. No PR mode exists (GitHub adapter
   is out of scope).

4. **Integration order is changeset creation order.** Deriving order from
   an item-reference graph needs LSP-grade analysis (Phase 3). The board
   states "creation order (no declared dependencies)" as the reason.

5. **A failing gate makes a changeset red**, not yellow. Spec Section 4
   mentions failing gates under both yellow ("a gate is failing") and red
   ("its merge rehearsal broke a gate"); a failed gate means the rehearsed
   tree is broken, so red. Yellow is reserved for "merges cleanly but not
   yet proven": gates pending, or touched outside the claim.

6. **Claim-less changesets are "unclaimed", not "outside claim".**
   Claimed-vs-actual flags outside touches only when the assignment holds
   claims. Radar-mode registrations with no claims at all would otherwise
   be permanently yellow, which contradicts radar mode needing no
   cooperation.

7. **Gate results from a joint rehearsal are attributed only to
   changesets that actually merged into that rehearsal tree.** A
   changeset conflicted out of the tree does not inherit the verdict. A
   targeted `gate_run` rehearses in order up to and including the target.

8. **Uncommitted work is simulated via read-only snapshot commits**:
   a temporary `GIT_INDEX_FILE`, `read-tree HEAD`, `add -A`, `write-tree`,
   `commit-tree`. The user's index, branches, and files are never touched;
   snapshots become dangling objects. This is what lets radar flag
   collisions before anything is committed.

9. **Tower's rehearsal scratch worktree is excluded from radar.** It is
   Tower-owned (`~/.tower/worktrees/<repo-hash>/rehearsal`), not peer
   work; scanning it double-reported conflicts it had already integrated.
   Managed worker worktrees under the same tree are still scanned.

10. **`tree-sitter` pinned to 0.25.** `tree-sitter-rust` 0.24 emits
    grammar ABI 15 while `tree-sitter` 0.24 accepts up to 14, which made
    Rust indexing silently empty. Caught by
    `rust_and_ts_items_are_extracted_with_lines`.

11. **`TOWER_HOME` environment variable** overrides `~/.tower/` so tests
    and the demo run fully isolated in scratch directories.

12. **`[reserved]`, `[escalation] extra_triggers`, and `[limits]` config
    sections are parsed (and written by `init`) but not enforced in v1.**
    Escalations are explicit objects via CLI/MCP. Enforcement is Phase 3
    work; the README states this instead of implying coverage.

13. **`tower task kill` sets task state `failed`** and closes its open
    assignments (releasing their claims). No worktree or branch is deleted
    (safety rule 1).

## Phase 2.1 (playtest fixes F1–F6)

14. **Statuses are order-aware (F1).** A changeset is red only for conflicts
    with main or *earlier* unlanded, non-superseded changesets; conflicts
    with later changesets are recorded as an informational "blocks C-x in
    <files>" reason. Alternative not taken: rehearsing the whole ordered
    chain to attribute conflicts — pairwise simulation plus position in
    integration order gives the same blocking answer far more cheaply.
    This intentionally changed two v1 test/demo assertions ("C-1 is red
    because C-2 conflicts with it"; demo now lands-refuses red C-2 and
    counts 18 MCP tools). Radar worktree collisions stay symmetric: they
    are raw worktree facts, not changeset verdicts.

15. **Landing candidates are built in a second Tower-owned scratch (F5).**
    `~/.tower/worktrees/<repo-hash>/landing`, separate from `rehearsal`,
    both excluded from radar. Green + fast-forward keeps the exact v1
    fast-forward mechanics; everything else landable goes through the
    candidate (merge onto current main, gates on the candidate tree,
    moved-head check with one rebuild, then `--ff-only` in the checked-out
    main worktree or a compare-and-swap `update-ref`). A yellow is
    landable only when every blocker is a pending/stale gate proof —
    checked against the live claimed-vs-actual plus reason classes, and
    failing closed on any unrecognized reason. A dirty checked-out main
    worktree still refuses outright (v1 rule) instead of falling back to
    `update-ref`, which would desync that worktree.

16. **Gate verdicts carry proof fields (F6).** Additive `gate_runs`
    columns `tree_hash`, `main_head`, `participants_json`. A pass counts
    only while the recorded main head is current and every recorded
    participant's current snapshot matches; otherwise yellow "gate proof
    stale …". Rows written by v1 (all three NULL) are grandfathered as
    current — invalidating every pre-existing pass on upgrade would make
    old homes spuriously yellow, and their next gate run records proof.
    `git snapshot_commit` now uses fixed author/committer dates so the
    same dirty tree snapshots to the same commit and proofs are
    comparable. The latest verdict per tier decides: a latest failure is
    red regardless of proof, a stale pass is yellow.

17. **Claimed-vs-actual is live; versions bump only on explicit refresh
    (F2).** Status refresh recomputes files/items/CVA from live claims and
    the live diff (base = merge-base with current main) and silently
    stores changes; `changeset refresh` additionally bumps `version`,
    records the head, and emits `changeset_refreshed`. So claim expansion
    alone can clear an outside-claim yellow at the next refresh, while
    `version` remains a meaningful "operator refreshed this" counter.

18. **Supersede is a flag plus a terminal status (F4).**
    `changesets.superseded`/`superseded_by` columns plus
    `status='superseded'`. Set by re-registration (same assignment),
    landing (same task), or `changeset supersede`. Superseded records are
    skipped by integration order, status refresh, and gate attribution
    (`gate_run` on one errors); radar pairing skips worktrees whose
    changesets are *all* superseded, so a manually superseded worktree
    stops producing phantom collisions while raw unregistered worktrees
    keep full radar coverage. Landing/refreshing a landed or superseded
    changeset refuses.

19. **Partial claim grants (F3) keep `collisions` as an alias of
    `refused`.** Each refusal carries both the v1 key `assignment_id` and
    `holding_assignment` (plus `file`, `item`, `held_item`,
    `requested_item`) so v1 consumers keep parsing. `granted` stays
    all-or-nothing; the CLI exits 1 only on zero granted (an empty request
    exits 0). Re-acquiring a node the same assignment already holds is a
    grant without a duplicate row. Per-node release matches the node
    exactly (`file` + `item`, NULL for file-level); omitted/empty node
    lists keep the v1 release-all behavior on both CLI and MCP.

20. **Board `next` is a pure function** of (status, reasons, landed,
    superseded) in `board::next_action`, shared by JSON/text/HTML boards
    and `changeset status`. Peer conflicts render as "resolve conflict
    with C-x in <files>" (the blocking peer is always the earlier one
    under decision 14); stale proofs outrank outside-claim when both are
    present, since rerunning gates is the cheaper unblock.

## Phase 2.2 (deep-playtest fixes N1–N3)

21. **Landing never releases claims; claims belong to the assignment
    (Fix A / N1, semantic change).** Before 2.2, `finalize_landing`
    silently deleted the landing assignment's claims, so a continuing
    assignment's next changeset flagged territory it had legitimately
    claimed in earlier rounds as outside-claim, and the board's CLAIMS
    section emptied while assignments were still open. Landing now
    touches no claims. They are released only by an explicit
    `claim release` or by `assignment close` (verified present:
    `Store::close_assignment` already deleted the assignment's claim
    rows; `task kill` still releases the same way, by closing its
    assignments). Both the `land` JSON receipt and the
    `changeset_landed` event payload carry `claims_held`
    (`[{"file", "item"}]`, and the event also gains `assignment_id`), so
    claim state after a landing is explicit in the receipt instead of
    silently empty. Alternative not taken: releasing with a warning —
    the deep playtest showed continuations are the normal case, so a
    warning would be routine friction for correct behavior. The
    now-dead `Store::release_claims` wrapper was removed outright
    (per the owner's no-shims ruling); `release_claims_nodes(.., None)`
    is the single release path. No pre-existing test or demo asserted
    release-on-land (verified by search), so no assertion had to be
    reworded for this change.

22. **Inline Rust modules are containers, not items (Fix B / N2).**
    The old Rust walker emitted each `mod_item` as one item, collapsing
    a whole `#[cfg(test)] mod tests` into a synthetic item named
    `tests`; any test edit then counted as touching `tests`, and no
    per-function claim could ever cover a normal change that included
    tests. The walker now descends into a module's `declaration_list`
    and indexes its contents directly: each test function is its own
    item, named by the function, exactly like a non-test function. A
    bodiless `mod foo;` declaration still yields a `module` item.
    Generalizing to *all* inline modules (rather than special-casing
    `#[cfg(test)]` or the name `tests`) was chosen because the collapse
    bug is identical for any inline module, and it leaves no name- or
    attribute-based special case anywhere — there is no `file#tests`
    case in claimed-vs-actual matching. TypeScript scope check: the TS
    indexer has **no analogous synthetic item** — it only ever emitted
    real declarations, and callback-style tests (`describe`/`it`) were
    never items at all. It is left unchanged; a TS file edited only in
    test callbacks has no changed items and is tracked at file
    granularity (stated in the README limitations).

23. **Claimed-vs-actual is file-scoped, and the outside detail is printed
    where the hint is (Fix C / N2).** Changed items are now carried as
    `(file, item)` pairs end to end (`live_diff` → `claimed_vs_actual`),
    and coverage requires a claim *in the item's own file* (that item's
    claim, or a whole-file claim on that file). Two pre-2.2 flattening
    bugs fall out of this: a whole-file claim on one file no longer
    "covers" items in every file, and a same-named item claim in
    another file no longer covers either. `outside_items` entries
    changed shape from bare item-name strings to `{"file", "item"}`
    objects, matching `outside_files` — a deliberate breaking change
    under the owner's ruling, since the bare string threw away the one
    fact (which file) the user needs in order to claim. A file with no
    claims at all is reported once, as a whole outside file; its items
    are not repeated in `outside_items`. Status reasons are generated
    through `engine::outside_reason`, e.g. "touched files outside
    claim: src/orders.rs#discount_is_tenth, src/notify.rs (whole
    file)" (items first, then whole files); the text board additionally
    expands any stored pre-2.2 plain reason from the row's
    claimed-vs-actual when rendering, so old rows read correctly
    without a rewrite (claims/diffs are recomputed live on refresh
    anyway, decision 17). `changeset status` has no separate text
    mode — JSON is its output — so CLI and MCP now share one builder
    (`board::changeset_status_value`) that returns every previous field
    plus `claimed_vs_actual` in exactly the `board --json` shape,
    ending the CLI/MCP copy-paste that let the two drift apart.

24. **Conflict detail is read-only inspection over classic
    `git merge-tree` (Fix D / N3).** New `tower changeset conflicts
    <ID> [--json]` and MCP `changeset_conflicts` (surface 18 → 19
    tools). The conflict *set* is computed exactly like the status's
    red reasons — main plus earlier unlanded changesets only, so an
    earlier changeset that merely "blocks" a later one reports no
    conflicts, and landed/superseded changesets report an empty list —
    with the `--write-tree` simulation (`merge_sim`) remaining the
    authority on which files conflict. Hunk headers, however, come
    from the classic three-argument `git merge-tree <base> <a> <b>`
    form, whose "changed in both" sections carry the conflict diffs;
    `--write-tree` output names files but contains no `@@` lines to
    show (both forms verified against git 2.43). Each entry is
    `{"against", "file", "hunks"}`; a conflicting file with no content
    hunks (e.g. modify/delete) is still named, with an empty hunk
    list, rather than dropped. Default CLI output is text (file lines
    with indented `@@` headers, `C-x: no conflicts` when empty)
    because the command's job is human inspection mid-resolution;
    `--json` and MCP return the structured value. Nothing is merged
    or resolved and no worktree is written — resolution stays in the
    worker's worktree, as everywhere else in Tower.

## Phase 2.3 (self-reporting: Tower files issues about itself)

25. **Self-reports live in a store outbox and are filed via `gh`,
    best-effort and silent.** A new additive `reports` table (signature,
    kind, title, body, context JSON, state queued/filed, occurrence
    count, first/last seen, filed URL/at, last commented threshold,
    unique per repo+signature) is the single outbox
    (`src/feedback.rs` owns the logic; `Store::upsert_report` /
    `mark_report_filed` own the rows). Filing shells out to
    `gh issue create --repo <feedback-repo> --label tower-auto-report
    --label <kind>` with a `<!-- tower-signature: <sig> -->` trailer;
    the binary is `TOWER_GH` or `gh` on PATH, which is also the test
    seam (stub scripts log argv and print a fake URL, or exit nonzero
    to simulate no-auth). Flush happens immediately after a report is
    created, opportunistically at the start of `board` / `changeset
    status` (CLI and MCP), and via `tower report flush`; a missing,
    unauthenticated, or offline `gh` leaves reports queued and never
    blocks or fails the user's operation. The feedback repo is
    `Holy-Coders/tower` by default, overridable in `[feedback]`
    (`repo`, `enabled`); `enabled = false` records locally and never
    files. Explicit entry points are `tower report --kind bug|inefficiency
    --title ... --body ... [--command ...]` (plus `report list` /
    `report flush` / `report create`) and MCP `report_create`
    (surface 19 → 20 tools), whose description fences reports to
    Tower itself, never the orchestrated code. Crate version 0.2.0.
    Alternative not taken: a direct GitHub API client — a new HTTP/TLS
    dependency plus token management Tower otherwise has no need for;
    `gh` already owns auth.

26. **Auto-capture classifies errors, catches panics at the boundary,
    and times operations — and can never change the outcome.** CLI
    `main` and MCP `dispatch` wrap their inner dispatch in
    `catch_unwind`, timing, and error inspection. A panic drafts a
    `bug` report and is then resumed unchanged. An error result drafts
    a `bug` report only when `feedback::is_internal_error` judges it
    Tower's fault: expected user errors (unknown IDs, unregistered
    repo, missing arguments, refused claims, held leases, non-landable
    changesets) are excluded first, then internal markers (panic,
    invariant, contradiction/mismatch, database, merge-tree/git
    internal failures) must be present. Operations over the 5-second
    threshold draft an `inefficiency` report, excluding `watch` (a
    loop by design), the long-running `serve-mcp` process (its
    individual tool calls are timed instead), and `report` itself (so
    filing can never recursively report on filing). The threshold is
    overridable via `TOWER_SLOW_REPORT_MS` for tests/demos; the
    production default stays 5s. Every auto-capture path discards its
    own result, so the triggering command's exit and output are
    untouched (pinned by the failing-gh integration test). Alternative
    not taken: reporting every error — refused claims and unknown IDs
    are Tower working as designed and would drown the real signal.

27. **Dedupe is a normalized signature, coalesced locally, adopted
    remotely, and capped per UTC day.** The signature is an FNV-1a
    hash (the codebase's existing hash style, cf. `config::repo_hash`)
    of kind + normalized title + normalized command key, where
    normalization lowercases, collapses whitespace, and replaces digit
    runs and long hex tokens with `#`, so varying Tower IDs, counts,
    and durations coalesce. A repeat increments the existing row's
    occurrence count and last-seen whether queued or filed; crossing
    10/100/1000 occurrences on a filed report posts one comment per
    crossed threshold via `gh issue comment` (best-effort, tracked in
    `last_commented_threshold`). Before creating, one
    `gh issue list --repo <repo> --label tower-auto-report --state open
    --limit 100` per flush (plus a `--json url,title,body` variant
    when supported, since the plain listing has no bodies to match the
    signature marker in) is searched for the signature; a hit adopts
    that issue's URL instead of opening a duplicate. At most 5 issues
    are filed per repo per UTC day (day boundary is UTC midnight, so
    the cap needs no timezone dependency); beyond it reports stay
    queued with their counts, never dropped.

28. **Scrubbing runs before storage, and fails closed on sensitive
    names.** Titles, bodies, error text, MCP argument dumps, and
    captured command strings all pass through `feedback::scrub`
    before anything is persisted: the repo's absolute prefix is
    replaced with repo-relative form, the home-directory prefix with
    `~/`, and the value of any flag, `name=value`, or `name: value`
    pair whose name contains key/token/secret/password/auth
    (case-insensitive substring — so `--api-key`, `API_KEY=`, and
    spelled-out variants all match) becomes `[redacted]`, including
    the following token of a `--flag value` pair. Auto context carries
    only version, OS/arch, timestamp, kind, recent Tower IDs, and the
    scrubbed command — never file contents, diffs, code, secrets, or
    environment values; a planted-fake-secret unit test asserts the
    secret appears neither in scrubbed output nor in the generated
    issue body. Alternative not taken: redacting only at filing time —
    the outbox itself would then persist secrets at rest.
