use std::path::{Path, PathBuf};

use crate::board;
use crate::config::TowerConfig;
use crate::engine;
use crate::git;
use crate::index;
use crate::mcp;
use crate::store::{ClaimNode, Store};

// ---------- helpers ----------

fn git_ok(dir: &Path, args: &[&str]) {
    let (code, out, err) = git::run_git_raw(dir, args);
    assert_eq!(code, 0, "git {:?} in {} failed: {} {}", args, dir.display(), out, err);
}

fn write(path: &Path, content: &str) {
    if let Some(p) = path.parent() { std::fs::create_dir_all(p).unwrap(); }
    std::fs::write(path, content).unwrap();
}

const MATH: &str = "export function add(a: number, b: number): number {\n  return a + b;\n}\nexport function mul(a: number, b: number): number {\n  return a * b;\n}\n";
const GREET: &str = "export function greet(name: string): string {\n  return `hello ${name}`;\n}\n";
const OTHER: &str = "export function other(): number {\n  return 1;\n}\n";
const TESTSH: &str = "#!/bin/sh\ngrep -q 'hello' src/greet.ts && grep -q 'a + b' src/math.ts\n";

struct Fixture {
    _tmp: tempfile::TempDir,
    root: PathBuf,
    repo: PathBuf,
    store: Store,
    rid: i64,
    cfg: TowerConfig,
}

/// Throwaway git repo on branch `main` with three TS files and a trivial
/// gate; registered in a store whose home is inside the temp dir.
fn fixture(with_gates: bool) -> Fixture {
    let tmp = tempfile::tempdir().unwrap();
    let root = tmp.path().to_path_buf();
    let repo = root.join("repo");
    std::fs::create_dir_all(&repo).unwrap();
    git_ok(&repo, &["init", "-b", "main"]);
    git_ok(&repo, &["config", "user.email", "t@t"]);
    git_ok(&repo, &["config", "user.name", "T"]);
    write(&repo.join("src/math.ts"), MATH);
    write(&repo.join("src/greet.ts"), GREET);
    write(&repo.join("src/other.ts"), OTHER);
    write(&repo.join("test.sh"), TESTSH);
    if with_gates {
        write(&repo.join("tower.toml"), "[repo]\nmain_branch = \"main\"\n\n[gates]\ncheck = \"sh test.sh\"\ntest = \"sh test.sh\"\n");
    }
    git_ok(&repo, &["add", "-A"]);
    git_ok(&repo, &["commit", "-m", "base"]);
    let store = Store::open(root.join("home")).unwrap();
    let cfg = TowerConfig::load(&repo);
    let r = store.register_repo(&git::canonical_str(&repo), "main").unwrap();
    Fixture { _tmp: tmp, root, repo, store, rid: r.id, cfg }
}

fn add_worktree(fx: &Fixture, name: &str, branch: &str) -> PathBuf {
    let dir = fx.root.join(name);
    git_ok(&fx.repo, &["worktree", "add", "-b", branch, &dir.to_string_lossy(), "main"]);
    git_ok(&dir, &["config", "user.email", "t@t"]);
    git_ok(&dir, &["config", "user.name", "T"]);
    dir
}

fn commit_all(dir: &Path, msg: &str) {
    git_ok(dir, &["add", "-A"]);
    git_ok(dir, &["commit", "-m", msg]);
}

fn replace_in(path: &Path, from: &str, to: &str) {
    let s = std::fs::read_to_string(path).unwrap();
    assert!(s.contains(from), "{} does not contain {:?}", path.display(), from);
    std::fs::write(path, s.replacen(from, to, 1)).unwrap();
}

/// task + assignment + (optional claims) + changeset for one worktree.
fn register(fx: &Fixture, wt: &Path, claims: &[ClaimNode]) -> (String, String, String) {
    let wt_s = wt.to_string_lossy().into_owned();
    let task = fx.store.create_task(fx.rid, "task", "brief").unwrap();
    let asg = fx.store.create_assignment(fx.rid, &task.id, &wt_s, "b").unwrap();
    if !claims.is_empty() {
        assert!(fx.store.acquire_claims(fx.rid, &asg.id, claims).unwrap().is_ok());
    }
    let v = engine::register_changeset(&fx.store, fx.rid, &fx.repo, &fx.cfg, &asg.id, &wt_s).unwrap();
    (task.id, asg.id, v["changeset_id"].as_str().unwrap().to_string())
}

fn status(fx: &Fixture, cid: &str) -> String {
    fx.store.get_changeset(fx.rid, cid).unwrap().unwrap().status
}

// ---------- store ----------

#[test]
fn ids_are_sequential_per_repo_with_documented_prefixes() {
    let s = Store::open_memory().unwrap();
    let r1 = s.register_repo("/tmp/repo-a", "main").unwrap();
    let r2 = s.register_repo("/tmp/repo-b", "main").unwrap();
    assert_eq!(s.create_task(r1.id, "one", "").unwrap().id, "T-1");
    assert_eq!(s.create_task(r1.id, "two", "").unwrap().id, "T-2");
    // Per-repo counters: the second repo starts over at T-1.
    assert_eq!(s.create_task(r2.id, "other", "").unwrap().id, "T-1");
    assert_eq!(s.create_assignment(r1.id, "T-1", "/wt", "b").unwrap().id, "A-1");
    assert_eq!(s.create_escalation(r1.id, "T-1", "q?", &serde_json::json!([]), "").unwrap().id, "E-1");
    assert_eq!(s.log_decision(r1.id, "decide").unwrap().id, "D-1");
    // Every state change appended an event; cursor paging works.
    let ev = s.events_since(r1.id, 0).unwrap();
    assert!(ev.len() >= 5, "events: {:?}", ev.iter().map(|e| &e.kind).collect::<Vec<_>>());
    let last = ev.last().unwrap().seq;
    assert!(s.events_since(r1.id, last).unwrap().is_empty());
    assert_eq!(s.list_repos().unwrap().len(), 2);
}

#[test]
fn lease_is_single_holder() {
    let s = Store::open_memory().unwrap();
    let r = s.register_repo("/tmp/repo-lease", "main").unwrap();
    assert!(s.lease_acquire(r.id, "host-a").unwrap().is_ok());
    assert!(s.lease_acquire(r.id, "host-a").unwrap().is_ok(), "same holder re-acquires");
    let refused = s.lease_acquire(r.id, "host-b").unwrap();
    assert_eq!(refused.unwrap_err(), "host-a");
    s.lease_release(r.id, "host-a").unwrap();
    assert!(s.lease_acquire(r.id, "host-b").unwrap().is_ok());
}

#[test]
fn escalation_and_decision_roundtrip() {
    let s = Store::open_memory().unwrap();
    let r = s.register_repo("/tmp/repo-ed", "main").unwrap();
    let e = s.create_escalation(r.id, "T-1", "which?", &serde_json::json!(["A", "B"]), "A").unwrap();
    assert_eq!(e.id, "E-1");
    s.resolve_escalation(r.id, "E-1", "B").unwrap();
    let e2 = s.get_escalation(r.id, "E-1").unwrap().unwrap();
    assert!(e2.resolved && e2.answer.as_deref() == Some("B"));
    let d = s.log_decision(r.id, "use B").unwrap();
    s.revoke_decision(r.id, &d.id).unwrap();
    assert!(s.list_decisions(r.id).unwrap()[0].revoked);
}

// ---------- claims (acceptance: conflict refused, colliding assignment named) ----------

#[test]
fn conflicting_claim_is_refused_and_names_the_holder() {
    let s = Store::open_memory().unwrap();
    let r = s.register_repo("/tmp/repo-claims", "main").unwrap();
    s.create_task(r.id, "t1", "").unwrap();
    s.create_task(r.id, "t2", "").unwrap();
    let a1 = s.create_assignment(r.id, "T-1", "/wt1", "b1").unwrap().id;
    let a2 = s.create_assignment(r.id, "T-2", "/wt2", "b2").unwrap().id;

    // A-1 claims one item; disjoint item in the same file is still free.
    assert!(s.acquire_claims(r.id, &a1, &[ClaimNode { file: "src/math.ts".into(), item: Some("add".into()) }]).unwrap().is_ok());
    assert!(s.acquire_claims(r.id, &a2, &[ClaimNode { file: "src/math.ts".into(), item: Some("mul".into()) }]).unwrap().is_ok());

    // Same item: refused, and the refusal names A-1 exactly.
    let cols = s.acquire_claims(r.id, &a2, &[ClaimNode { file: "src/math.ts".into(), item: Some("add".into()) }]).unwrap().unwrap_err();
    assert_eq!(cols.len(), 1);
    assert_eq!(cols[0]["assignment_id"], serde_json::json!("A-1"));
    assert_eq!(cols[0]["held_item"], serde_json::json!("add"));

    // File-level claim collides with any item claim in that file.
    let cols = s.acquire_claims(r.id, &a2, &[ClaimNode { file: "src/math.ts".into(), item: None }]).unwrap().unwrap_err();
    assert!(cols.iter().any(|c| c["assignment_id"] == serde_json::json!("A-1")));

    // The refused claim was not recorded; release frees the territory.
    assert_eq!(s.claims_for(r.id, &a2).unwrap().len(), 1);
    s.release_claims_nodes(r.id, &a1, None).unwrap();
    assert!(s.acquire_claims(r.id, &a2, &[ClaimNode { file: "src/math.ts".into(), item: Some("add".into()) }]).unwrap().is_ok());
}

// ---------- tree-sitter index ----------

#[test]
fn rust_and_ts_items_are_extracted_with_lines() {
    let items = index::extract_items("lib.rs", "fn alpha() {}\nstruct Beta;\nimpl Beta {\n  fn gamma(&self) {}\n}\n");
    let names: Vec<&str> = items.iter().map(|i| i.name.as_str()).collect();
    assert!(names.contains(&"alpha"), "{names:?}");
    assert!(names.contains(&"Beta"));
    assert!(names.contains(&"Beta::gamma"), "{names:?}");
    assert!(items.iter().all(|i| i.start_line >= 1 && i.end_line >= i.start_line && i.file == "lib.rs"));

    let items = index::extract_items("a.ts", "export function add() {}\nexport class Box {\n  open() {}\n}\nconst arrow = () => 1;\n");
    let names: Vec<&str> = items.iter().map(|i| i.name.as_str()).collect();
    assert!(names.contains(&"add"), "{names:?}");
    assert!(names.contains(&"Box"));
    assert!(names.contains(&"Box.open"), "{names:?}");
    assert!(names.contains(&"arrow"));

    // Unknown language: file-level granularity (no items).
    assert!(index::extract_items("x.py", "def f():\n    pass\n").is_empty());
}

#[test]
fn changed_items_only_flags_edited_items() {
    let base = "fn a() { 1 }\nfn b() { 2 }\n";
    let new = "fn a() { 1 }\nfn b() { 3 }\nfn c() { 4 }\n";
    let changed = index::changed_items("x.rs", Some(base), Some(new));
    assert_eq!(changed, vec!["b".to_string(), "c".to_string()]);
    assert_eq!(index::changed_items("x.rs", Some(base), Some(base)), Vec::<String>::new());
}

// ---------- claimed vs actual ----------

#[test]
fn claimed_vs_actual_distinguishes_outside_from_unclaimed() {
    // Phase 2.2 shape change (Fix C): changed items are (file, item) pairs
    // and outside_items entries are {"file", "item"} objects, so coverage
    // and reporting are file-scoped.
    let claims = vec![ClaimNode { file: "src/greet.ts".into(), item: Some("greet".into()) }];
    let changed: Vec<(String, String)> = vec![("src/greet.ts".into(), "greet".into()), ("src/math.ts".into(), "mul".into())];
    let v = engine::claimed_vs_actual(&claims, &["src/greet.ts".into(), "src/math.ts".into()], &changed);
    assert_eq!(v["outside_files"].as_array().unwrap().len(), 1);
    assert_eq!(v["outside_files"][0]["file"], serde_json::json!("src/math.ts"));
    // A wholly outside file is reported once, as a file; its items are not
    // repeated in outside_items.
    assert!(v["outside_items"].as_array().unwrap().is_empty(), "{v}");
    assert!(engine::outside_claim(&v));

    // Editing an unclaimed item in a claimed file is outside too.
    let v = engine::claimed_vs_actual(&claims, &["src/greet.ts".into()], &[("src/greet.ts".into(), "farewell".into())]);
    assert_eq!(v["outside_items"], serde_json::json!([{"file": "src/greet.ts", "item": "farewell"}]));
    assert!(engine::outside_claim(&v));
    // The reason text derived from the same value names both forms.
    assert_eq!(engine::outside_reason(&v), "touched files outside claim: src/greet.ts#farewell");

    // A claim held in a different file does not cover a same-named item:
    // coverage is scoped to the item's own file.
    let other_file = vec![ClaimNode { file: "src/other.ts".into(), item: Some("farewell".into()) }, ClaimNode { file: "src/greet.ts".into(), item: Some("greet".into()) }];
    let v = engine::claimed_vs_actual(&other_file, &["src/greet.ts".into()], &[("src/greet.ts".into(), "farewell".into())]);
    assert!(engine::outside_claim(&v), "claim in another file must not cover: {v}");

    // No claims at all: reported as unclaimed, never flagged outside.
    let v = engine::claimed_vs_actual(&[], &["src/greet.ts".into()], &[("src/greet.ts".into(), "greet".into())]);
    assert!(v["outside_files"].as_array().unwrap().is_empty());
    assert_eq!(v["unclaimed_files"].as_array().unwrap().len(), 1);
    assert!(!engine::outside_claim(&v));
}

// ---------- config tiers ----------

#[test]
fn repo_without_gates_is_honest_tier0_only() {
    let tmp = tempfile::tempdir().unwrap();
    let cfg = TowerConfig::load(tmp.path());
    assert!(!cfg.has_gates());
    assert!(cfg.tier_label().contains("Tier-0 simulation only"));
    write(&tmp.path().join("tower.toml"), "[gates]\ncheck = \"true\"\ntest = \"true\"\n");
    let cfg = TowerConfig::load(tmp.path());
    assert!(cfg.has_gates() && cfg.tier_label().contains("Tier-2"));
}

// ---------- radar (git) ----------

fn conflicting_pair() -> (Fixture, PathBuf, PathBuf, PathBuf) {
    let fx = fixture(true);
    let w1 = add_worktree(&fx, "w1", "w1");
    let w2 = add_worktree(&fx, "w2", "w2");
    let w3 = add_worktree(&fx, "w3", "w3");
    replace_in(&w1.join("src/math.ts"), "return a + b;", "return a + b + 0;");
    replace_in(&w2.join("src/math.ts"), "return a + b;", "const s = a + b;\n  return s;");
    replace_in(&w3.join("src/other.ts"), "return 1;", "return 2;");
    commit_all(&w1, "w1");
    commit_all(&w2, "w2");
    commit_all(&w3, "w3");
    (fx, w1, w2, w3)
}

#[test]
fn radar_flags_conflicting_worktrees_red_and_unrelated_green() {
    let (fx, w1, w2, w3) = conflicting_pair();
    let radar = engine::radar_scan(&fx.store, &fx.repo, &fx.cfg).unwrap();
    assert_eq!(radar.collisions.len(), 1, "{:?}", radar.collisions);
    let col = &radar.collisions[0];
    assert_eq!(col.files, vec!["src/math.ts".to_string()]);
    let paths: Vec<&str> = vec![col.a.as_str(), col.b.as_str()];
    assert!(paths.contains(&w1.to_string_lossy().as_ref()) && paths.contains(&w2.to_string_lossy().as_ref()), "collision names both worktrees: {paths:?}");
    let st = |p: &Path| radar.worktrees.iter().find(|w| w.path == p.to_string_lossy()).unwrap().status.clone();
    assert_eq!(st(&w1), "red");
    assert_eq!(st(&w2), "red");
    assert_eq!(st(&w3), "green");
    // Item-level diff indexing saw the edited TS function.
    let scan3 = radar.worktrees.iter().find(|w| w.path == w3.to_string_lossy()).unwrap();
    assert!(scan3.changed_item_names.contains(&"other".to_string()), "{:?}", scan3.changed_item_names);
}

#[test]
fn changesets_mirror_radar_status_with_named_counterparties() {
    let (fx, w1, w2, w3) = conflicting_pair();
    let (_, _, c1) = register(&fx, &w1, &[]);
    let (_, _, c2) = register(&fx, &w2, &[]);
    let (_, _, c3) = register(&fx, &w3, &[ClaimNode { file: "src/other.ts".into(), item: Some("other".into()) }]);
    assert_eq!((c1.as_str(), c2.as_str(), c3.as_str()), ("C-1", "C-2", "C-3"));
    // Order-aware (F1, intentional change): C-1 is first in integration
    // order, so the conflict with later C-2 is informational only; C-2 is
    // the one blocked, naming C-1.
    assert_eq!(status(&fx, "C-1"), "yellow", "gates pending, not blocked by later C-2");
    assert_eq!(status(&fx, "C-2"), "red");
    let c1r = fx.store.get_changeset(fx.rid, "C-1").unwrap().unwrap();
    assert!(c1r.reasons_json.contains("blocks C-2"), "info reason names the blocked later changeset: {}", c1r.reasons_json);
    let c2r = fx.store.get_changeset(fx.rid, "C-2").unwrap().unwrap();
    assert!(c2r.reasons_json.contains("conflicts with C-1"), "blocking reason names the earlier changeset: {}", c2r.reasons_json);
    // C-3 is clean but gates have not run yet -> yellow; after gates, green.
    assert_eq!(status(&fx, "C-3"), "yellow");
    engine::gate_run_single(&fx.store, fx.rid, &fx.repo, &fx.cfg, "C-3", 1).unwrap();
    engine::gate_run_single(&fx.store, fx.rid, &fx.repo, &fx.cfg, "C-3", 2).unwrap();
    assert_eq!(status(&fx, "C-3"), "green");
    // Gates attributed to C-3 only, and the board labels the collision by C-ID.
    assert!(fx.store.gate_runs_for(fx.rid, "C-3").unwrap().iter().all(|g| g.passed));
    let b = board::build_board(&fx.store, fx.rid, &fx.repo, &fx.cfg).unwrap();
    assert!(b["collisions"].as_array().unwrap().iter().any(|c| c["a_label"] == serde_json::json!("C-1") && c["b_label"] == serde_json::json!("C-2")));
}

// ---------- safety (acceptance): rehearsal must not touch user work ----------

fn snapshot_user_state(repo: &Path, wts: &[PathBuf]) -> Vec<(String, String, String)> {
    // (worktree, branch tip per local branch, bytes of a marker file)
    let branches = git::run_git(repo, &["show-ref", "--heads"]).unwrap_or_default();
    wts.iter()
        .map(|w| (w.to_string_lossy().into_owned(), branches.clone(), std::fs::read_to_string(w.join("src/math.ts")).unwrap_or_default()))
        .collect()
}

#[test]
fn rehearsal_leaves_user_branches_and_worktree_files_byte_identical() {
    let (fx, w1, w2, w3) = conflicting_pair();
    // An uncommitted edit sitting in a user worktree must survive untouched.
    replace_in(&w1.join("src/greet.ts"), "hello ${name}", "hi ${name}");
    let (_, _, _c1) = register(&fx, &w1, &[]);
    let (_, _, _c3) = register(&fx, &w3, &[ClaimNode { file: "src/other.ts".into(), item: None }]);
    let wts = vec![fx.repo.clone(), w1.clone(), w2.clone(), w3.clone()];
    let before = snapshot_user_state(&fx.repo, &wts);
    let head_before: Vec<String> = wts.iter().map(|w| git::rev_parse(w, "HEAD").unwrap()).collect();
    let greet_before = std::fs::read_to_string(w1.join("src/greet.ts")).unwrap();
    let status_before: Vec<String> = wts.iter().map(|w| git::run_git(w, &["status", "--porcelain"]).unwrap_or_default()).collect();

    let v = engine::rehearse(&fx.store, fx.rid, &fx.repo, &fx.cfg, &[1, 2]).unwrap();
    assert!(v["integration_head"].as_str().map(|s| !s.is_empty()).unwrap_or(false));
    // Integration happened on Tower's scratch ref only.
    assert!(git::rev_parse(&fx.repo, "refs/tower/integration").is_ok());

    // Radar must not treat Tower's own rehearsal scratch worktree as peer work.
    let radar = engine::radar_scan(&fx.store, &fx.repo, &fx.cfg).unwrap();
    assert!(radar.worktrees.iter().all(|w| !w.path.ends_with("rehearsal")), "{:?}", radar.worktrees.iter().map(|w| &w.path).collect::<Vec<_>>());
    assert_eq!(snapshot_user_state(&fx.repo, &wts), before, "branch tips / worktree files changed!");
    assert_eq!(wts.iter().map(|w| git::rev_parse(w, "HEAD").unwrap()).collect::<Vec<_>>(), head_before);
    assert_eq!(std::fs::read_to_string(w1.join("src/greet.ts")).unwrap(), greet_before, "uncommitted user edit modified!");
    assert_eq!(wts.iter().map(|w| git::run_git(w, &["status", "--porcelain"]).unwrap_or_default()).collect::<Vec<_>>(), status_before);
}

// ---------- outside claim + land ----------

#[test]
fn touching_outside_claim_is_yellow_and_cannot_land() {
    let fx = fixture(true);
    let w = add_worktree(&fx, "w", "w");
    replace_in(&w.join("src/greet.ts"), "hello ${name}", "hello ${name.trim()}");
    replace_in(&w.join("src/math.ts"), "return a * b;", "return a * b * 1;");
    commit_all(&w, "work");
    // Claimed only greet; touched math.ts as well.
    let (_, _, cid) = register(&fx, &w, &[ClaimNode { file: "src/greet.ts".into(), item: Some("greet".into()) }]);
    let cs = fx.store.get_changeset(fx.rid, &cid).unwrap().unwrap();
    let cva: serde_json::Value = serde_json::from_str(&cs.claimed_vs_actual_json).unwrap();
    assert_eq!(cva["outside_files"][0]["file"], serde_json::json!("src/math.ts"));
    assert_eq!(status(&fx, &cid), "yellow");
    let err = engine::land_changeset(&fx.store, fx.rid, &fx.repo, &fx.cfg, &cid).unwrap_err();
    assert!(format!("{err}").contains("non-green"), "{err}");
}

#[test]
fn land_fast_forwards_green_and_refuses_red() {
    let (fx, w1, w2, _w3) = conflicting_pair();
    register(&fx, &w1, &[]);
    register(&fx, &w2, &[]);
    // Order-aware: C-2 is the red one (blocked by earlier C-1); red cannot land.
    assert_eq!(status(&fx, "C-2"), "red");
    assert!(engine::land_changeset(&fx.store, fx.rid, &fx.repo, &fx.cfg, "C-2").is_err());
    // Clean candidate on an untouched file: gates -> green -> lands (FF).
    // (Yellow-with-only-gate-proofs is now also landable via a candidate —
    // intentional F5 change, covered by the candidate-landing tests.)
    let w4 = add_worktree(&fx, "w4", "w4");
    replace_in(&w4.join("src/greet.ts"), "hello ${name}", "hello ${name.trim()}");
    commit_all(&w4, "trim");
    let (tid, _, cid) = register(&fx, &w4, &[ClaimNode { file: "src/greet.ts".into(), item: Some("greet".into()) }]);
    assert_eq!(status(&fx, &cid), "yellow", "gates pending");
    engine::gate_run_single(&fx.store, fx.rid, &fx.repo, &fx.cfg, &cid, 1).unwrap();
    engine::gate_run_single(&fx.store, fx.rid, &fx.repo, &fx.cfg, &cid, 2).unwrap();
    assert_eq!(status(&fx, &cid), "green");
    let v = engine::land_changeset(&fx.store, fx.rid, &fx.repo, &fx.cfg, &cid).unwrap();
    assert_eq!(v["landed"], serde_json::json!(true));
    assert_eq!(v["method"], serde_json::json!("fast-forward"));
    let on_main = git::run_git(&fx.repo, &["show", "main:src/greet.ts"]).unwrap();
    assert!(on_main.contains("name.trim()"));
    assert_eq!(fx.store.get_task(fx.rid, &tid).unwrap().unwrap().state, "landed");
}

#[test]
fn gate_failure_blocks_green_and_land() {
    let fx = fixture(true);
    let w = add_worktree(&fx, "w", "w");
    // Breaks the gate: greet no longer says hello.
    replace_in(&w.join("src/greet.ts"), "hello ${name}", "bye ${name}");
    commit_all(&w, "break");
    let (_, _, cid) = register(&fx, &w, &[ClaimNode { file: "src/greet.ts".into(), item: Some("greet".into()) }]);
    let g = engine::gate_run_single(&fx.store, fx.rid, &fx.repo, &fx.cfg, &cid, 1).unwrap();
    assert_eq!(g["passed"], serde_json::json!(false));
    assert_ne!(status(&fx, &cid), "green");
    assert!(engine::land_changeset(&fx.store, fx.rid, &fx.repo, &fx.cfg, &cid).is_err());
}

#[test]
fn tower_managed_worktree_flow_lands_without_gates() {
    // No tower.toml gates: Tier-0 only; clean changeset is green immediately.
    let fx = fixture(false);
    assert!(!fx.cfg.has_gates());
    let task = fx.store.create_task(fx.rid, "managed", "").unwrap();
    let v = crate::ops::worktree_create(&fx.store, fx.rid, &fx.repo, &fx.cfg, &task.id).unwrap();
    let wt = PathBuf::from(v["worktree"].as_str().unwrap());
    assert!(wt.starts_with(fx.store.home.join("worktrees")), "managed under ~/.tower: {}", wt.display());
    git_ok(&wt, &["config", "user.email", "t@t"]);
    git_ok(&wt, &["config", "user.name", "T"]);
    replace_in(&wt.join("src/other.ts"), "return 1;", "return 2;");
    commit_all(&wt, "work");
    let aid = v["assignment_id"].as_str().unwrap();
    fx.store.acquire_claims(fx.rid, aid, &[ClaimNode { file: "src/other.ts".into(), item: None }]).unwrap().unwrap();
    let reg = engine::register_changeset(&fx.store, fx.rid, &fx.repo, &fx.cfg, aid, &wt.to_string_lossy()).unwrap();
    assert_eq!(reg["status"], serde_json::json!("green"));
    let cid = reg["changeset_id"].as_str().unwrap();
    assert!(engine::land_changeset(&fx.store, fx.rid, &fx.repo, &fx.cfg, cid).is_ok());
    assert!(git::run_git(&fx.repo, &["show", "main:src/other.ts"]).unwrap().contains("return 2;"));
}

// ---------- MCP dispatch ----------

#[test]
fn mcp_tools_mirror_the_cli_surface() {
    let fx = fixture(true);
    let repo_s = fx.repo.to_string_lossy().into_owned();
    let call = |name: &str, args: serde_json::Value| mcp::dispatch(&fx.store, name, &args).unwrap();
    let v = call("task_create", serde_json::json!({"repo": repo_s, "title": "mcp task", "brief": "b"}));
    assert_eq!(v["task_id"], serde_json::json!("T-1"));
    assert_eq!(call("task_plan_save", serde_json::json!({"repo": repo_s, "task_id": "T-1", "plan": "p"}))["saved"], serde_json::json!(true));
    let wc = call("worktree_create", serde_json::json!({"repo": repo_s, "task_id": "T-1"}));
    assert_eq!(wc["assignment_id"], serde_json::json!("A-1"));
    let wt = wc["worktree"].as_str().unwrap().to_string();
    let granted = call("claim_acquire", serde_json::json!({"repo": repo_s, "assignment_id": "A-1", "nodes": [{"file": "src/other.ts"}]}));
    assert_eq!(granted["granted"], serde_json::json!(true));
    let b = call("board_get", serde_json::json!({"repo": repo_s}));
    assert_eq!(b["tasks"].as_array().unwrap().len(), 1);
    let ev = call("events_since", serde_json::json!({"repo": repo_s, "cursor": 0}));
    assert!(ev["events"].as_array().unwrap().len() >= 4);
    let order = call("integration_order", serde_json::json!({"repo": repo_s}));
    assert!(order["order"].is_array());
    let listed = call("worktree_list", serde_json::json!({"repo": repo_s}));
    assert!(listed.as_array().unwrap().iter().any(|w| w["path"] == serde_json::json!(wt)));
    let e = call("escalate_create", serde_json::json!({"repo": repo_s, "task_id": "T-1", "question": "?", "options": ["A"], "recommendation": "A"}));
    assert_eq!(e["escalation_id"], serde_json::json!("E-1"));
    assert_eq!(call("escalate_resolve", serde_json::json!({"repo": repo_s, "escalation_id": "E-1", "answer": "A"}))["resolved"], serde_json::json!(true));
    let d = call("decision_log", serde_json::json!({"repo": repo_s, "text": "go with A"}));
    assert_eq!(d["decision_id"], serde_json::json!("D-1"));
    assert_eq!(call("decision_list", serde_json::json!({"repo": repo_s}))["decisions"].as_array().unwrap().len(), 1);
    assert!(mcp::dispatch(&fx.store, "not_a_tool", &serde_json::json!({"repo": repo_s})).is_err());
    // tools/list advertises the 16 spec tools + Phase 2.1
    // changeset_refresh/supersede + Phase 2.2 changeset_conflicts.
    assert_eq!(mcp::tool_defs().as_array().unwrap().len(), 19);
}

// ---------- merge simulation primitive ----------

#[test]
fn merge_sim_reports_clean_and_conflict_files() {
    let fx = fixture(false);
    let w1 = add_worktree(&fx, "w1", "w1");
    let w2 = add_worktree(&fx, "w2", "w2");
    replace_in(&w1.join("src/math.ts"), "return a + b;", "return a + b + 0;");
    replace_in(&w2.join("src/math.ts"), "return a + b;", "return a + b + 1;");
    commit_all(&w1, "w1");
    commit_all(&w2, "w2");
    let main = git::rev_parse(&fx.repo, "main").unwrap();
    let h1 = git::rev_parse(&w1, "HEAD").unwrap();
    let h2 = git::rev_parse(&w2, "HEAD").unwrap();
    assert!(git::merge_sim(&fx.repo, &main, &main, &h1).clean);
    let sim = git::merge_sim(&fx.repo, &main, &h1, &h2);
    assert!(!sim.clean);
    assert_eq!(sim.conflict_files, vec!["src/math.ts".to_string()]);
}

// ---------- Phase 2.1 ----------

fn reasons(fx: &Fixture, cid: &str) -> String {
    fx.store.get_changeset(fx.rid, cid).unwrap().unwrap().reasons_json
}

#[test]
fn order_aware_first_of_conflicting_pair_is_green_after_gates_second_red() {
    let (fx, w1, w2, _w3) = conflicting_pair();
    register(&fx, &w1, &[]);
    register(&fx, &w2, &[]);
    engine::gate_run_single(&fx.store, fx.rid, &fx.repo, &fx.cfg, "C-1", 1).unwrap();
    engine::gate_run_single(&fx.store, fx.rid, &fx.repo, &fx.cfg, "C-1", 2).unwrap();
    assert_eq!(status(&fx, "C-1"), "green", "first in order is landable: {}", reasons(&fx, "C-1"));
    assert!(reasons(&fx, "C-1").contains("blocks C-2 in src/math.ts"), "{}", reasons(&fx, "C-1"));
    assert_eq!(status(&fx, "C-2"), "red");
    assert!(reasons(&fx, "C-2").contains("conflicts with C-1 in src/math.ts"), "{}", reasons(&fx, "C-2"));
    // Board next-actions follow from the same state.
    let b = board::build_board(&fx.store, fx.rid, &fx.repo, &fx.cfg).unwrap();
    let next = |id: &str| b["changesets"].as_array().unwrap().iter().find(|c| c["id"] == serde_json::json!(id)).unwrap()["next"].as_str().unwrap().to_string();
    assert_eq!(next("C-1"), "land");
    assert_eq!(next("C-2"), "resolve conflict with C-1 in src/math.ts");
    // And the green first changeset really can land (no pre-integration).
    assert!(engine::land_changeset(&fx.store, fx.rid, &fx.repo, &fx.cfg, "C-1").is_ok());
    assert!(git::run_git(&fx.repo, &["show", "main:src/math.ts"]).unwrap().contains("return a + b + 0;"));
}

#[test]
fn candidate_landing_lands_green_non_fast_forward_and_leaves_worker_untouched() {
    let fx = fixture(true);
    let w1 = add_worktree(&fx, "w1", "w1");
    let w2 = add_worktree(&fx, "w2", "w2");
    replace_in(&w1.join("src/greet.ts"), "hello ${name}", "hello ${name.trim()}");
    replace_in(&w2.join("src/other.ts"), "return 1;", "return 2;");
    commit_all(&w1, "greet");
    commit_all(&w2, "other");
    let (_, _, c1) = register(&fx, &w1, &[ClaimNode { file: "src/greet.ts".into(), item: Some("greet".into()) }]);
    let (_, _, c2) = register(&fx, &w2, &[ClaimNode { file: "src/other.ts".into(), item: Some("other".into()) }]);
    engine::gate_run_single(&fx.store, fx.rid, &fx.repo, &fx.cfg, &c1, 1).unwrap();
    engine::gate_run_single(&fx.store, fx.rid, &fx.repo, &fx.cfg, &c1, 2).unwrap();
    engine::land_changeset(&fx.store, fx.rid, &fx.repo, &fx.cfg, &c1).unwrap();
    let based_main = git::rev_parse(&fx.repo, "main").unwrap();
    // C-2 forked from the old main: no longer a fast-forward, but compatible.
    let source_head = git::rev_parse(&w2, "HEAD").unwrap();
    assert_ne!(git::merge_base(&fx.repo, &based_main, &source_head).unwrap(), based_main);
    engine::gate_run_single(&fx.store, fx.rid, &fx.repo, &fx.cfg, &c2, 1).unwrap();
    engine::gate_run_single(&fx.store, fx.rid, &fx.repo, &fx.cfg, &c2, 2).unwrap();
    assert_eq!(status(&fx, &c2), "green", "{}", reasons(&fx, &c2));
    let greet_before = std::fs::read_to_string(w2.join("src/greet.ts")).unwrap();
    let other_before = std::fs::read_to_string(w2.join("src/other.ts")).unwrap();
    let v = engine::land_changeset(&fx.store, fx.rid, &fx.repo, &fx.cfg, &c2).unwrap();
    assert_eq!(v["landed"], serde_json::json!(true));
    assert_eq!(v["method"], serde_json::json!("candidate"));
    assert_eq!(v["source_head"].as_str().unwrap(), source_head);
    assert_eq!(v["based_on_main_head"].as_str().unwrap(), based_main);
    let candidate_head = v["candidate_head"].as_str().unwrap().to_string();
    assert_ne!(candidate_head, source_head, "candidate is a Tower-built merge commit");
    assert_eq!(git::rev_parse(&fx.repo, "main").unwrap(), candidate_head);
    assert!(git::run_git(&fx.repo, &["show", "main:src/other.ts"]).unwrap().contains("return 2;"));
    assert!(git::run_git(&fx.repo, &["show", "main:src/greet.ts"]).unwrap().contains("name.trim()"));
    // Worker worktree byte-identical: same HEAD, same files (no merge done there).
    assert_eq!(git::rev_parse(&w2, "HEAD").unwrap(), source_head);
    assert_eq!(std::fs::read_to_string(w2.join("src/greet.ts")).unwrap(), greet_before);
    assert_eq!(std::fs::read_to_string(w2.join("src/other.ts")).unwrap(), other_before);
    assert!(!git::is_dirty(&w2));
    // Landed event records source, candidate, and based-on heads.
    let ev = fx.store.events_since(fx.rid, 0).unwrap();
    let landed_ev = ev.iter().filter(|e| e.kind == "changeset_landed").filter_map(|e| serde_json::from_str::<serde_json::Value>(&e.payload_json).ok()).find(|p| p["changeset_id"] == serde_json::json!(c2)).unwrap();
    assert_eq!(landed_ev["source_head"].as_str().unwrap(), source_head);
    assert_eq!(landed_ev["candidate_head"].as_str().unwrap(), candidate_head);
    assert_eq!(landed_ev["based_on_main_head"].as_str().unwrap(), based_main);
    // Gates on the candidate were recorded with proof fields.
    let tree = git::rev_parse(&fx.repo, &format!("{candidate_head}^{{tree}}")).unwrap();
    assert!(fx.store.gate_runs_for(fx.rid, &c2).unwrap().iter().any(|g| g.tree_hash.as_deref() == Some(tree.as_str()) && g.main_head.as_deref() == Some(based_main.as_str()) && g.participants_json.as_deref().map(|p| p.contains(&c2) && p.contains(&source_head)).unwrap_or(false)), "candidate gate proof recorded");
}

#[test]
fn candidate_conflict_fails_red_with_files_and_main_unchanged() {
    let fx = fixture(true);
    let w1 = add_worktree(&fx, "w1", "w1");
    replace_in(&w1.join("src/math.ts"), "return a + b;", "return a + b + 0;");
    commit_all(&w1, "w1");
    let (_, _, c1) = register(&fx, &w1, &[]);
    // Main advances externally with a conflicting edit to the same line.
    replace_in(&fx.repo.join("src/math.ts"), "return a + b;", "const s = a + b;\n  return s;");
    commit_all(&fx.repo, "main conflicting");
    let main_before = git::rev_parse(&fx.repo, "main").unwrap();
    let source_head = git::rev_parse(&w1, "HEAD").unwrap();
    // The candidate builder itself reports the conflict and aborts cleanly.
    match engine::build_landing_candidate(&fx.store, fx.rid, &fx.repo, &fx.cfg, &c1, &source_head, &main_before, false).unwrap() {
        engine::Candidate::Conflict { files } => assert_eq!(files, vec!["src/math.ts".to_string()]),
        _ => panic!("expected candidate conflict"),
    }
    let scratch = engine::landing_scratch_dir(&fx.store, &git::canonical_str(&fx.repo));
    assert_eq!(git::rev_parse(&scratch, "HEAD").unwrap(), main_before, "aborted candidate scratch reset to main");
    // A landing attempt fails, the changeset is red naming the file, main unmoved.
    let err = engine::land_changeset(&fx.store, fx.rid, &fx.repo, &fx.cfg, &c1).unwrap_err();
    assert!(format!("{err}").contains("src/math.ts"), "{err}");
    assert_eq!(status(&fx, &c1), "red");
    assert!(reasons(&fx, &c1).contains("src/math.ts"), "{}", reasons(&fx, &c1));
    assert_eq!(git::rev_parse(&fx.repo, "main").unwrap(), main_before);
    assert_eq!(git::rev_parse(&w1, "HEAD").unwrap(), source_head);
}

#[test]
fn candidate_gate_failure_marks_red_and_leaves_main_unchanged() {
    let fx = fixture(true);
    let w1 = add_worktree(&fx, "w1", "w1");
    let w2 = add_worktree(&fx, "w2", "w2");
    replace_in(&w1.join("src/other.ts"), "return 1;", "return 2;");
    // Breaks the gate on the candidate tree (greet no longer says hello).
    replace_in(&w2.join("src/greet.ts"), "hello ${name}", "bye ${name}");
    commit_all(&w1, "other");
    commit_all(&w2, "break");
    let (_, _, c1) = register(&fx, &w1, &[ClaimNode { file: "src/other.ts".into(), item: None }]);
    let (_, _, c2) = register(&fx, &w2, &[ClaimNode { file: "src/greet.ts".into(), item: Some("greet".into()) }]);
    // Yellow (gates pending) is landable: the candidate runs gates itself.
    let v = engine::land_changeset(&fx.store, fx.rid, &fx.repo, &fx.cfg, &c1).unwrap();
    assert_eq!(v["method"], serde_json::json!("candidate"));
    let main_after_c1 = git::rev_parse(&fx.repo, "main").unwrap();
    let w2_head = git::rev_parse(&w2, "HEAD").unwrap();
    let err = engine::land_changeset(&fx.store, fx.rid, &fx.repo, &fx.cfg, &c2).unwrap_err();
    assert!(format!("{err}").contains("gate"), "{err}");
    assert_eq!(status(&fx, &c2), "red");
    assert!(reasons(&fx, &c2).contains("gate failed"), "{}", reasons(&fx, &c2));
    assert_eq!(git::rev_parse(&fx.repo, "main").unwrap(), main_after_c1);
    assert_eq!(git::rev_parse(&w2, "HEAD").unwrap(), w2_head);
}

#[test]
fn stale_gate_proof_drops_green_to_yellow_until_gates_rerun() {
    let fx = fixture(true);
    let w1 = add_worktree(&fx, "w1", "w1");
    let w2 = add_worktree(&fx, "w2", "w2");
    replace_in(&w1.join("src/greet.ts"), "hello ${name}", "hello ${name.trim()}");
    replace_in(&w2.join("src/other.ts"), "return 1;", "return 2;");
    commit_all(&w1, "greet");
    commit_all(&w2, "other");
    let (_, _, c1) = register(&fx, &w1, &[ClaimNode { file: "src/greet.ts".into(), item: Some("greet".into()) }]);
    let (_, _, c2) = register(&fx, &w2, &[ClaimNode { file: "src/other.ts".into(), item: Some("other".into()) }]);
    // Joint rehearsal: each pass is bound to both participants + main head.
    engine::rehearse(&fx.store, fx.rid, &fx.repo, &fx.cfg, &[1, 2]).unwrap();
    assert_eq!(status(&fx, &c1), "green", "{}", reasons(&fx, &c1));
    assert_eq!(status(&fx, &c2), "green", "{}", reasons(&fx, &c2));
    let g = &fx.store.gate_runs_for(fx.rid, &c1).unwrap()[0];
    assert!(g.tree_hash.is_some() && g.main_head.is_some() && g.participants_json.as_deref().unwrap().contains(&c2));
    // Peer snapshot changes -> C-1's proof is stale.
    replace_in(&w2.join("src/other.ts"), "return 2;", "return 3;");
    commit_all(&w2, "other v2");
    engine::refresh_changeset_statuses(&fx.store, fx.rid, &fx.repo, &fx.cfg).unwrap();
    assert_eq!(status(&fx, &c1), "yellow");
    assert!(reasons(&fx, &c1).contains("gate proof stale"), "{}", reasons(&fx, &c1));
    // Rerunning gates for C-1 restores green.
    engine::gate_run_single(&fx.store, fx.rid, &fx.repo, &fx.cfg, &c1, 1).unwrap();
    engine::gate_run_single(&fx.store, fx.rid, &fx.repo, &fx.cfg, &c1, 2).unwrap();
    assert_eq!(status(&fx, &c1), "green", "{}", reasons(&fx, &c1));
    // Main moving also stales the proof.
    write(&fx.repo.join("NOTES.txt"), "note\n");
    commit_all(&fx.repo, "notes on main");
    engine::refresh_changeset_statuses(&fx.store, fx.rid, &fx.repo, &fx.cfg).unwrap();
    assert_eq!(status(&fx, &c1), "yellow");
    assert!(reasons(&fx, &c1).contains("gate proof stale"), "{}", reasons(&fx, &c1));
    let b = board::build_board(&fx.store, fx.rid, &fx.repo, &fx.cfg).unwrap();
    let n = b["changesets"].as_array().unwrap().iter().find(|c| c["id"] == serde_json::json!(c1)).unwrap()["next"].as_str().unwrap().to_string();
    assert_eq!(n, "rerun gates (proof stale)");
}

#[test]
fn changeset_refresh_clears_outside_yellow_and_reregister_supersedes() {
    let fx = fixture(true);
    let w = add_worktree(&fx, "w", "w");
    replace_in(&w.join("src/greet.ts"), "hello ${name}", "hello ${name.trim()}");
    replace_in(&w.join("src/math.ts"), "return a * b;", "return a * b * 1;");
    commit_all(&w, "work");
    let (_, aid, c1) = register(&fx, &w, &[ClaimNode { file: "src/greet.ts".into(), item: Some("greet".into()) }]);
    assert_eq!(status(&fx, &c1), "yellow");
    assert!(reasons(&fx, &c1).contains("outside claim"));
    assert_eq!(fx.store.get_changeset(fx.rid, &c1).unwrap().unwrap().version, 1);
    // Expand claims, move the branch head, then refresh instead of re-registering.
    let r = fx.store.acquire_claims_partial(fx.rid, &aid, &[ClaimNode { file: "src/math.ts".into(), item: Some("mul".into()) }]).unwrap();
    assert_eq!(r.granted_nodes.len(), 1);
    replace_in(&w.join("src/greet.ts"), "name.trim()", "name.trimStart()");
    commit_all(&w, "work v2");
    let new_head = git::rev_parse(&w, "HEAD").unwrap();
    let v = engine::refresh_changeset(&fx.store, fx.rid, &fx.repo, &fx.cfg, &c1).unwrap();
    assert_eq!(v["version"], serde_json::json!(2));
    assert_eq!(v["head"].as_str().unwrap(), new_head);
    assert_eq!(v["claimed_vs_actual"]["outside_files"].as_array().unwrap().len(), 0);
    assert_eq!(v["claimed_vs_actual"]["outside_items"].as_array().unwrap().len(), 0);
    assert!(!reasons(&fx, &c1).contains("outside claim"), "{}", reasons(&fx, &c1));
    assert!(fx.store.events_since(fx.rid, 0).unwrap().iter().any(|e| e.kind == "changeset_refreshed"));
    // Re-registering the assignment supersedes the old record everywhere.
    let reg2 = engine::register_changeset(&fx.store, fx.rid, &fx.repo, &fx.cfg, &aid, &w.to_string_lossy()).unwrap();
    let c2 = reg2["changeset_id"].as_str().unwrap().to_string();
    assert_ne!(c2, c1);
    let old = fx.store.get_changeset(fx.rid, &c1).unwrap().unwrap();
    assert!(old.superseded && old.status == "superseded" && old.superseded_by.as_deref() == Some(c2.as_str()));
    let order = engine::integration_order(&fx.store, fx.rid).unwrap();
    assert!(order.iter().all(|o| o["changeset_id"] != serde_json::json!(c1)));
    assert!(order.iter().any(|o| o["changeset_id"] == serde_json::json!(c2)));
    let b = board::build_board(&fx.store, fx.rid, &fx.repo, &fx.cfg).unwrap();
    assert!(b["changesets"].as_array().unwrap().iter().all(|c| c["id"] != serde_json::json!(c1)));
    assert!(b["superseded_changesets"].as_array().unwrap().iter().any(|c| c["id"] == serde_json::json!(c1) && c["superseded_by"] == serde_json::json!(c2)));
    assert!(board::render_text(&b).contains("SUPERSEDED CHANGESETS"));
    engine::refresh_changeset_statuses(&fx.store, fx.rid, &fx.repo, &fx.cfg).unwrap();
    assert_eq!(fx.store.get_changeset(fx.rid, &c1).unwrap().unwrap().status, "superseded", "refresh must not resurrect it");
    // MCP mirrors: refresh on a superseded changeset errors; manual supersede works.
    let repo_s = fx.repo.to_string_lossy().into_owned();
    assert!(mcp::dispatch(&fx.store, "changeset_refresh", &serde_json::json!({"repo": repo_s, "changeset_id": c1})).is_err());
    let sup = mcp::dispatch(&fx.store, "changeset_supersede", &serde_json::json!({"repo": repo_s, "changeset_id": c2})).unwrap();
    assert_eq!(sup["superseded"], serde_json::json!(true));
}

#[test]
fn landing_supersedes_sibling_changesets_of_same_task() {
    let fx = fixture(true);
    let w1 = add_worktree(&fx, "w1", "w1");
    let w2 = add_worktree(&fx, "w2", "w2");
    replace_in(&w1.join("src/greet.ts"), "hello ${name}", "hello ${name.trim()}");
    replace_in(&w2.join("src/other.ts"), "return 1;", "return 2;");
    commit_all(&w1, "greet");
    commit_all(&w2, "other");
    let task = fx.store.create_task(fx.rid, "both", "").unwrap();
    let mut cids = Vec::new();
    for (wt, node) in [(&w1, ClaimNode { file: "src/greet.ts".into(), item: Some("greet".into()) }), (&w2, ClaimNode { file: "src/other.ts".into(), item: Some("other".into()) })] {
        let asg = fx.store.create_assignment(fx.rid, &task.id, &wt.to_string_lossy(), "b").unwrap();
        fx.store.acquire_claims_partial(fx.rid, &asg.id, &[node]).unwrap();
        let v = engine::register_changeset(&fx.store, fx.rid, &fx.repo, &fx.cfg, &asg.id, &wt.to_string_lossy()).unwrap();
        cids.push(v["changeset_id"].as_str().unwrap().to_string());
    }
    let (c1, c2) = (cids[0].clone(), cids[1].clone());
    engine::gate_run_single(&fx.store, fx.rid, &fx.repo, &fx.cfg, &c1, 1).unwrap();
    engine::gate_run_single(&fx.store, fx.rid, &fx.repo, &fx.cfg, &c1, 2).unwrap();
    assert_eq!(status(&fx, &c1), "green");
    engine::land_changeset(&fx.store, fx.rid, &fx.repo, &fx.cfg, &c1).unwrap();
    let sibling = fx.store.get_changeset(fx.rid, &c2).unwrap().unwrap();
    assert!(sibling.superseded && sibling.superseded_by.as_deref() == Some(c1.as_str()), "{sibling:?}");
    assert!(fx.store.events_since(fx.rid, 0).unwrap().iter().any(|e| e.kind == "changeset_superseded" && e.payload_json.contains(&c2)));
    assert!(engine::integration_order(&fx.store, fx.rid).unwrap().is_empty());
}

#[test]
fn partial_claim_grants_free_nodes_and_refuses_held_by_name() {
    let s = Store::open_memory().unwrap();
    let r = s.register_repo("/tmp/repo-partial", "main").unwrap();
    s.create_task(r.id, "t1", "").unwrap();
    s.create_task(r.id, "t2", "").unwrap();
    let a1 = s.create_assignment(r.id, "T-1", "/wt1", "b1").unwrap().id;
    let a2 = s.create_assignment(r.id, "T-2", "/wt2", "b2").unwrap().id;
    let first = s.acquire_claims_partial(r.id, &a1, &[ClaimNode { file: "src/math.ts".into(), item: Some("add".into()) }]).unwrap();
    assert_eq!(first.granted_nodes.len(), 1);
    assert!(first.refused.is_empty());
    // Mixed bundle: free nodes granted, held node refused by name.
    let mixed = s.acquire_claims_partial(r.id, &a2, &[ClaimNode { file: "src/math.ts".into(), item: Some("add".into()) }, ClaimNode { file: "src/greet.ts".into(), item: Some("greet".into()) }, ClaimNode { file: "src/other.ts".into(), item: None }]).unwrap();
    assert_eq!(mixed.granted_nodes.len(), 2, "granted the uncontested subset");
    assert!(mixed.granted_nodes.iter().any(|n| n.file == "src/greet.ts"));
    assert!(mixed.granted_nodes.iter().any(|n| n.file == "src/other.ts"));
    assert_eq!(mixed.refused.len(), 1);
    assert_eq!(mixed.refused[0]["file"], serde_json::json!("src/math.ts"));
    assert_eq!(mixed.refused[0]["item"], serde_json::json!("add"));
    assert_eq!(mixed.refused[0]["assignment_id"], serde_json::json!("A-1"));
    assert_eq!(mixed.refused[0]["holding_assignment"], serde_json::json!("A-1"));
    assert_eq!(s.claims_for(r.id, &a2).unwrap().len(), 2, "refused node was not recorded");
    // Fully refused bundle grants nothing.
    let none = s.acquire_claims_partial(r.id, &a2, &[ClaimNode { file: "src/math.ts".into(), item: Some("add".into()) }]).unwrap();
    assert!(none.granted_nodes.is_empty());
    assert_eq!(none.refused.len(), 1);
    assert_eq!(s.claims_for(r.id, &a2).unwrap().len(), 2);
}

#[test]
fn per_node_claim_release_frees_exactly_one_node() {
    let s = Store::open_memory().unwrap();
    let r = s.register_repo("/tmp/repo-release", "main").unwrap();
    s.create_task(r.id, "t1", "").unwrap();
    s.create_task(r.id, "t2", "").unwrap();
    let a1 = s.create_assignment(r.id, "T-1", "/wt1", "b1").unwrap().id;
    let a2 = s.create_assignment(r.id, "T-2", "/wt2", "b2").unwrap().id;
    let nodes = vec![ClaimNode { file: "src/math.ts".into(), item: Some("add".into()) }, ClaimNode { file: "src/greet.ts".into(), item: Some("greet".into()) }, ClaimNode { file: "src/other.ts".into(), item: None }];
    assert_eq!(s.acquire_claims_partial(r.id, &a1, &nodes).unwrap().granted_nodes.len(), 3);
    s.release_claims_nodes(r.id, &a1, Some(&[ClaimNode { file: "src/greet.ts".into(), item: Some("greet".into()) }])).unwrap();
    let left = s.claims_for(r.id, &a1).unwrap();
    assert_eq!(left.len(), 2);
    assert!(left.iter().all(|n| n.file != "src/greet.ts"));
    // The freed node can be taken; the still-held one cannot.
    let gained = s.acquire_claims_partial(r.id, &a2, &[ClaimNode { file: "src/greet.ts".into(), item: Some("greet".into()) }]).unwrap();
    assert_eq!(gained.granted_nodes.len(), 1);
    let still_held = s.acquire_claims_partial(r.id, &a2, &[ClaimNode { file: "src/math.ts".into(), item: Some("add".into()) }]).unwrap();
    assert!(still_held.granted_nodes.is_empty() && still_held.refused.len() == 1);
    // Release without nodes still releases everything for that assignment.
    s.release_claims_nodes(r.id, &a1, None).unwrap();
    assert!(s.claims_for(r.id, &a1).unwrap().is_empty());
    assert_eq!(s.claims_for(r.id, &a2).unwrap().len(), 1);
}

#[test]
fn board_next_action_is_pure_function_of_status_and_reasons() {
    assert_eq!(board::next_action("green", &[], false, false), "land");
    assert_eq!(board::next_action("green", &[], true, false), "landed");
    assert_eq!(board::next_action("superseded", &[], false, true), "superseded");
    assert_eq!(board::next_action("yellow", &["Tier-1 check gate has not passed yet".into()], false, false), "run gates");
    assert_eq!(board::next_action("yellow", &[engine::STALE_REASON.to_string()], false, false), "rerun gates (proof stale)");
    assert_eq!(board::next_action("yellow", &["touched files outside claim".into()], false, false), "refresh claims or changeset");
    assert_eq!(board::next_action("red", &["conflicts with C-2 in src/x.rs".into()], false, false), "resolve conflict with C-2 in src/x.rs");
    assert_eq!(board::next_action("red", &["gate failed: cargo test".into()], false, false), "fix failing gate");
}

#[test]
fn v1_home_without_phase21_columns_migrates_in_place() {
    let tmp = tempfile::tempdir().unwrap();
    let home = tmp.path().join("home");
    std::fs::create_dir_all(&home).unwrap();
    {
        let conn = rusqlite::Connection::open(home.join("tower.db")).unwrap();
        conn.execute_batch(
            "CREATE TABLE repos(id INTEGER PRIMARY KEY, path TEXT UNIQUE NOT NULL, main_branch TEXT NOT NULL, created_at INTEGER NOT NULL);
             CREATE TABLE changesets(id TEXT NOT NULL, repo_id INTEGER NOT NULL, assignment_id TEXT NOT NULL, task_id TEXT NOT NULL, worktree_path TEXT NOT NULL, branch TEXT NOT NULL, base_commit TEXT NOT NULL, head_commit TEXT NOT NULL, files_json TEXT NOT NULL, items_json TEXT NOT NULL, claimed_vs_actual_json TEXT NOT NULL, status TEXT NOT NULL, reasons_json TEXT NOT NULL, created_at INTEGER NOT NULL, PRIMARY KEY(repo_id,id));
             CREATE TABLE gate_runs(id INTEGER PRIMARY KEY, repo_id INTEGER NOT NULL, changeset_id TEXT NOT NULL, tier INTEGER NOT NULL, command TEXT NOT NULL, passed INTEGER NOT NULL, output TEXT NOT NULL, created_at INTEGER NOT NULL);
             INSERT INTO repos VALUES(1,'/tmp/legacy-repo','main',1);
             INSERT INTO changesets VALUES('C-1',1,'A-1','T-1','/wt','b','base','head','[]','[]','{}','green','[]',1);
             INSERT INTO gate_runs VALUES(1,1,'C-1',1,'true',1,'ok',1);",
        ).unwrap();
    }
    let s = Store::open(home).unwrap();
    let c = s.get_changeset(1, "C-1").unwrap().unwrap();
    assert_eq!(c.version, 1);
    assert!(!c.superseded && c.superseded_by.is_none());
    let g = s.gate_runs_for(1, "C-1").unwrap();
    assert_eq!(g.len(), 1);
    assert!(g[0].tree_hash.is_none() && g[0].main_head.is_none() && g[0].participants_json.is_none());
    // Legacy proof-less passes are grandfathered as current.
    assert!(engine::gate_proof_current(&g[0], &Default::default(), "anything"));
}

#[test]
fn candidate_landing_uses_compare_and_swap_when_main_not_checked_out() {
    let fx = fixture(true);
    let w1 = add_worktree(&fx, "w1", "w1");
    let w2 = add_worktree(&fx, "w2", "w2");
    replace_in(&w1.join("src/greet.ts"), "hello ${name}", "hello ${name.trim()}");
    replace_in(&w2.join("src/other.ts"), "return 1;", "return 2;");
    commit_all(&w1, "greet");
    commit_all(&w2, "other");
    let (_, _, c1) = register(&fx, &w1, &[ClaimNode { file: "src/greet.ts".into(), item: None }]);
    let (_, _, c2) = register(&fx, &w2, &[ClaimNode { file: "src/other.ts".into(), item: None }]);
    // No worktree has main checked out any more.
    let base = git::rev_parse(&fx.repo, "main").unwrap();
    git_ok(&fx.repo, &["checkout", "-b", "side"]);
    engine::gate_run_single(&fx.store, fx.rid, &fx.repo, &fx.cfg, &c1, 1).unwrap();
    engine::gate_run_single(&fx.store, fx.rid, &fx.repo, &fx.cfg, &c1, 2).unwrap();
    let v1 = engine::land_changeset(&fx.store, fx.rid, &fx.repo, &fx.cfg, &c1).unwrap();
    assert_eq!(v1["method"], serde_json::json!("fast-forward"));
    assert_eq!(git::rev_parse(&fx.repo, "main").unwrap(), git::rev_parse(&w1, "HEAD").unwrap(), "CAS advanced the ref");
    // The repo worktree stayed on its side branch, files untouched.
    assert_eq!(git::rev_parse(&fx.repo, "HEAD").unwrap(), base);
    assert_eq!(git::current_branch(&fx.repo).as_deref(), Some("side"));
    assert!(!std::fs::read_to_string(fx.repo.join("src/greet.ts")).unwrap().contains("trim()"));
    engine::gate_run_single(&fx.store, fx.rid, &fx.repo, &fx.cfg, &c2, 1).unwrap();
    engine::gate_run_single(&fx.store, fx.rid, &fx.repo, &fx.cfg, &c2, 2).unwrap();
    assert_eq!(status(&fx, &c2), "green", "{}", reasons(&fx, &c2));
    let v2 = engine::land_changeset(&fx.store, fx.rid, &fx.repo, &fx.cfg, &c2).unwrap();
    assert_eq!(v2["method"], serde_json::json!("candidate"));
    assert_eq!(git::rev_parse(&fx.repo, "main").unwrap(), v2["candidate_head"].as_str().unwrap());
    assert!(git::run_git(&fx.repo, &["show", "main:src/other.ts"]).unwrap().contains("return 2;"));
    assert_eq!(git::rev_parse(&fx.repo, "HEAD").unwrap(), base, "side worktree still untouched");
}

#[test]
fn superseded_worktree_is_excluded_from_radar_pairing_and_gates() {
    let (fx, w1, w2, _w3) = conflicting_pair();
    let (_, _, c1) = register(&fx, &w1, &[]);
    // Before superseding, radar flags the w1<->w2 collision.
    assert_eq!(engine::radar_scan(&fx.store, &fx.repo, &fx.cfg).unwrap().collisions.len(), 1);
    engine::supersede_changeset(&fx.store, fx.rid, &fx.repo, &fx.cfg, &c1).unwrap();
    let radar = engine::radar_scan(&fx.store, &fx.repo, &fx.cfg).unwrap();
    assert!(radar.collisions.is_empty(), "superseded-only worktree no longer pairs: {:?}", radar.collisions);
    assert!(engine::gate_run_single(&fx.store, fx.rid, &fx.repo, &fx.cfg, &c1, 1).is_err(), "gates refuse superseded changesets");
    assert!(engine::land_changeset(&fx.store, fx.rid, &fx.repo, &fx.cfg, &c1).is_err(), "landing refuses superseded changesets");
    let _ = w2;
}

// ---------- Phase 2.2 ----------

#[test]
fn landing_does_not_release_claims_and_reports_claims_held() {
    // Fix A (N1): claims belong to the assignment, not the changeset.
    let fx = fixture(false);
    let w = add_worktree(&fx, "w", "w");
    replace_in(&w.join("src/other.ts"), "return 1;", "return 2;");
    commit_all(&w, "work");
    let node = ClaimNode { file: "src/other.ts".into(), item: Some("other".into()) };
    let (_, aid, cid) = register(&fx, &w, &[node.clone()]);
    assert_eq!(status(&fx, &cid), "green");
    let v = engine::land_changeset(&fx.store, fx.rid, &fx.repo, &fx.cfg, &cid).unwrap();
    assert_eq!(v["landed"], serde_json::json!(true));
    // The landing receipt names the claims still held.
    assert_eq!(v["claims_held"], serde_json::json!([{"file": "src/other.ts", "item": "other"}]), "land output: {v}");
    // And they really are still held afterwards.
    assert_eq!(fx.store.claims_for(fx.rid, &aid).unwrap(), vec![node]);
    assert_eq!(fx.store.list_claims(fx.rid).unwrap().len(), 1);
    // The landed event payload records the same list.
    let ev = fx.store.events_since(fx.rid, 0).unwrap();
    let landed_ev = ev.iter().filter(|e| e.kind == "changeset_landed").filter_map(|e| serde_json::from_str::<serde_json::Value>(&e.payload_json).ok()).find(|p| p["changeset_id"] == serde_json::json!(cid)).unwrap();
    assert_eq!(landed_ev["claims_held"], serde_json::json!([{"file": "src/other.ts", "item": "other"}]));
    // A continuing assignment can still land more work under the same claims:
    // they were never silently dropped, so nothing looks outside-claim.
    replace_in(&w.join("src/other.ts"), "return 2;", "return 3;");
    commit_all(&w, "work v2");
    let reg2 = engine::register_changeset(&fx.store, fx.rid, &fx.repo, &fx.cfg, &aid, &w.to_string_lossy()).unwrap();
    assert_eq!(reg2["claimed_vs_actual"]["outside_files"].as_array().unwrap().len(), 0);
    assert_eq!(reg2["claimed_vs_actual"]["outside_items"].as_array().unwrap().len(), 0);
}

#[test]
fn assignment_close_releases_all_of_the_assignments_claims() {
    let s = Store::open_memory().unwrap();
    let r = s.register_repo("/tmp/repo-close", "main").unwrap();
    s.create_task(r.id, "t1", "").unwrap();
    s.create_task(r.id, "t2", "").unwrap();
    let a1 = s.create_assignment(r.id, "T-1", "/wt1", "b1").unwrap().id;
    let a2 = s.create_assignment(r.id, "T-2", "/wt2", "b2").unwrap().id;
    let nodes = vec![ClaimNode { file: "src/math.ts".into(), item: Some("add".into()) }, ClaimNode { file: "src/other.ts".into(), item: None }];
    assert_eq!(s.acquire_claims_partial(r.id, &a1, &nodes).unwrap().granted_nodes.len(), 2);
    s.acquire_claims_partial(r.id, &a2, &[ClaimNode { file: "src/greet.ts".into(), item: None }]).unwrap();
    s.close_assignment(r.id, &a1).unwrap();
    assert!(s.claims_for(r.id, &a1).unwrap().is_empty(), "close releases every claim of the assignment");
    assert_eq!(s.claims_for(r.id, &a2).unwrap().len(), 1, "other assignments keep their claims");
    assert_eq!(s.get_assignment(r.id, &a1).unwrap().unwrap().state, "closed");
    // The freed territory is claimable again.
    assert_eq!(s.acquire_claims_partial(r.id, &a2, &[ClaimNode { file: "src/math.ts".into(), item: Some("add".into()) }]).unwrap().granted_nodes.len(), 1);
}

const RUST_ORDERS: &str = "pub fn total() -> i64 { 3 }\npub fn discount() -> i64 { 1 }\n\n#[cfg(test)]\nmod tests {\n    use super::*;\n\n    #[test]\n    fn total_is_three() {\n        assert_eq!(total(), 3);\n    }\n\n    #[test]\n    fn discount_is_one() {\n        assert_eq!(discount(), 1);\n    }\n}\n";

#[test]
fn rust_indexer_emits_individual_test_functions_not_a_tests_item() {
    // Fix B (N2): no synthetic `tests` item collapsing the test module.
    let items = index::extract_items("src/orders.rs", RUST_ORDERS);
    let names: Vec<&str> = items.iter().map(|i| i.name.as_str()).collect();
    assert!(names.contains(&"total"), "{names:?}");
    assert!(names.contains(&"total_is_three"), "each test fn is its own item: {names:?}");
    assert!(names.contains(&"discount_is_one"), "{names:?}");
    assert!(!names.contains(&"tests"), "no synthetic tests item: {names:?}");
    let t = items.iter().find(|i| i.name == "discount_is_one").unwrap();
    assert!(t.start_line >= 1 && t.end_line >= t.start_line && t.file == "src/orders.rs");
    // Editing one test function flags exactly that function as changed.
    let edited = RUST_ORDERS.replace("assert_eq!(discount(), 1);", "assert_eq!(discount(), 2);");
    assert_eq!(index::changed_items("src/orders.rs", Some(RUST_ORDERS), Some(&edited)), vec!["discount_is_one".to_string()]);
    assert_eq!(index::changed_items("src/orders.rs", Some(RUST_ORDERS), Some(RUST_ORDERS)), Vec::<String>::new());
}

#[test]
fn claim_on_one_test_function_does_not_cover_another() {
    // Fix B: with per-function test items, claims are per test function.
    let edited = RUST_ORDERS.replace("assert_eq!(discount(), 1);", "assert_eq!(discount(), 2);");
    let changed = index::changed_items("src/orders.rs", Some(RUST_ORDERS), Some(&edited));
    assert_eq!(changed, vec!["discount_is_one".to_string()]);
    let pairs: Vec<(String, String)> = changed.iter().map(|n| ("src/orders.rs".to_string(), n.clone())).collect();
    let files = vec!["src/orders.rs".to_string()];
    let other_test = vec![ClaimNode { file: "src/orders.rs".into(), item: Some("total_is_three".into()) }];
    let v = engine::claimed_vs_actual(&other_test, &files, &pairs);
    assert!(engine::outside_claim(&v), "claim on total_is_three must not cover discount_is_one: {v}");
    assert_eq!(v["outside_items"], serde_json::json!([{"file": "src/orders.rs", "item": "discount_is_one"}]));
    let this_test = vec![ClaimNode { file: "src/orders.rs".into(), item: Some("discount_is_one".into()) }];
    assert!(!engine::outside_claim(&engine::claimed_vs_actual(&this_test, &files, &pairs)));
    let whole_file = vec![ClaimNode { file: "src/orders.rs".into(), item: None }];
    assert!(!engine::outside_claim(&engine::claimed_vs_actual(&whole_file, &files, &pairs)), "whole-file claim covers test edits");
}

#[test]
fn changeset_status_carries_claimed_vs_actual_and_board_names_outside_items() {
    // Fix C (N2 output): the outside detail appears where the hint is printed.
    let fx = fixture(true);
    let w = add_worktree(&fx, "w", "w");
    replace_in(&w.join("src/math.ts"), "return a + b;", "return a + b + 0;");
    replace_in(&w.join("src/math.ts"), "return a * b;", "return a * b * 1;");
    replace_in(&w.join("src/greet.ts"), "hello ${name}", "hello ${name.trim()}");
    commit_all(&w, "work");
    // Claimed only `add`: `mul` is an outside item, greet.ts an outside file.
    let (_, _, cid) = register(&fx, &w, &[ClaimNode { file: "src/math.ts".into(), item: Some("add".into()) }]);
    assert_eq!(status(&fx, &cid), "yellow");
    let repo_s = fx.repo.to_string_lossy().into_owned();
    let st = mcp::dispatch(&fx.store, "changeset_status", &serde_json::json!({"repo": repo_s, "changeset_id": cid})).unwrap();
    assert_eq!(st["next"], serde_json::json!("refresh claims or changeset"));
    let cva = &st["claimed_vs_actual"];
    assert_eq!(cva["outside_items"], serde_json::json!([{"file": "src/math.ts", "item": "mul"}]), "status CVA names the outside item: {st}");
    assert_eq!(cva["outside_files"][0]["file"], serde_json::json!("src/greet.ts"));
    let reasons_s = st["reasons"].to_string();
    assert!(reasons_s.contains("touched files outside claim: src/math.ts#mul"), "reason names the outside item: {reasons_s}");
    assert!(reasons_s.contains("src/greet.ts (whole file)"), "reason names the outside file: {reasons_s}");
    // Same shape as board --json: status CVA equals the board's CVA.
    let b = board::build_board(&fx.store, fx.rid, &fx.repo, &fx.cfg).unwrap();
    let bc = b["changesets"].as_array().unwrap().iter().find(|c| c["id"] == serde_json::json!(cid)).unwrap();
    assert_eq!(&bc["claimed_vs_actual"], cva);
    // Text board names the outside items/files on the reason line.
    let text = board::render_text(&b);
    assert!(text.contains("touched files outside claim: src/math.ts#mul"), "text board:\n{text}");
    assert!(text.contains("src/greet.ts (whole file)"), "text board:\n{text}");
}

#[test]
fn changeset_conflicts_names_file_and_hunk_header() {
    // Fix D (N3): conflict detail without leaving Tower for raw git.
    let (fx, w1, w2, w3) = conflicting_pair();
    register(&fx, &w1, &[]);
    register(&fx, &w2, &[]);
    register(&fx, &w3, &[ClaimNode { file: "src/other.ts".into(), item: Some("other".into()) }]);
    assert_eq!(status(&fx, "C-2"), "red");
    let main_before = git::rev_parse(&fx.repo, "main").unwrap();
    let w2_head = git::rev_parse(&w2, "HEAD").unwrap();
    let v = engine::changeset_conflicts(&fx.store, fx.rid, &fx.repo, &fx.cfg, "C-2").unwrap();
    assert_eq!(v["changeset_id"], serde_json::json!("C-2"));
    let conflicts = v["conflicts"].as_array().unwrap();
    assert_eq!(conflicts.len(), 1, "one conflicting file vs the earlier changeset: {v}");
    assert_eq!(conflicts[0]["against"], serde_json::json!("C-1"));
    assert_eq!(conflicts[0]["file"], serde_json::json!("src/math.ts"));
    let hunks = conflicts[0]["hunks"].as_array().unwrap();
    assert!(hunks.iter().any(|h| h.as_str().map(|s| s.starts_with("@@")).unwrap_or(false)), "hunk headers from git merge-tree: {v}");
    let text = engine::render_conflicts_text(&v);
    assert!(text.contains("src/math.ts") && text.contains("@@"), "text form names file + hunk:\n{text}");
    // Earlier-in-order and clean changesets report an empty conflict list,
    // matching their (non-red) statuses.
    let v1 = engine::changeset_conflicts(&fx.store, fx.rid, &fx.repo, &fx.cfg, "C-1").unwrap();
    assert!(v1["conflicts"].as_array().unwrap().is_empty(), "C-1 only blocks C-2 informationally: {v1}");
    let v3 = engine::changeset_conflicts(&fx.store, fx.rid, &fx.repo, &fx.cfg, "C-3").unwrap();
    assert!(v3["conflicts"].as_array().unwrap().is_empty(), "{v3}");
    assert!(engine::render_conflicts_text(&v3).contains("no conflicts"));
    // MCP mirrors the same detail.
    let repo_s = fx.repo.to_string_lossy().into_owned();
    let mv = mcp::dispatch(&fx.store, "changeset_conflicts", &serde_json::json!({"repo": repo_s, "changeset_id": "C-2"})).unwrap();
    assert_eq!(mv["conflicts"][0]["file"], serde_json::json!("src/math.ts"));
    assert!(mv["conflicts"][0]["hunks"].as_array().unwrap().iter().any(|h| h.as_str().map(|s| s.starts_with("@@")).unwrap_or(false)));
    // Read-only: no heads moved and no worktree was written.
    assert_eq!(git::rev_parse(&fx.repo, "main").unwrap(), main_before);
    assert_eq!(git::rev_parse(&w2, "HEAD").unwrap(), w2_head);
    assert!(!git::is_dirty(&w2));
    assert!(engine::changeset_conflicts(&fx.store, fx.rid, &fx.repo, &fx.cfg, "C-99").is_err());
}

#[test]
fn changeset_conflicts_against_main_after_main_advances() {
    let fx = fixture(true);
    let w1 = add_worktree(&fx, "w1", "w1");
    replace_in(&w1.join("src/math.ts"), "return a + b;", "return a + b + 0;");
    commit_all(&w1, "w1");
    let (_, _, c1) = register(&fx, &w1, &[]);
    replace_in(&fx.repo.join("src/math.ts"), "return a + b;", "const s = a + b;\n  return s;");
    commit_all(&fx.repo, "main conflicting");
    let v = engine::changeset_conflicts(&fx.store, fx.rid, &fx.repo, &fx.cfg, &c1).unwrap();
    let conflicts = v["conflicts"].as_array().unwrap();
    assert_eq!(conflicts.len(), 1, "{v}");
    assert_eq!(conflicts[0]["against"], serde_json::json!("main"));
    assert_eq!(conflicts[0]["file"], serde_json::json!("src/math.ts"));
    assert!(conflicts[0]["hunks"].as_array().unwrap().iter().any(|h| h.as_str().map(|s| s.starts_with("@@")).unwrap_or(false)), "{v}");
}


