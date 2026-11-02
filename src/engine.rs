use anyhow::{bail, Context, Result};
use serde::{Deserialize, Serialize};
use std::path::{Path, PathBuf};

use crate::config::TowerConfig;
use crate::git;
use crate::index;
use crate::resources::{self, Reservation, ResourceGuard};
use crate::store::{ClaimNode, Store};

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct WorktreeScan {
    pub path: String,
    pub head: String,
    pub snapshot: String,
    pub branch: Option<String>,
    pub base: String,
    pub files: Vec<String>,
    pub items: Vec<index::ItemInfo>,
    pub changed_item_names: Vec<String>,
    pub status: String,
    pub reasons: Vec<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Collision { pub a: String, pub b: String, pub files: Vec<String> }

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct RadarResult {
    pub worktrees: Vec<WorktreeScan>,
    pub collisions: Vec<Collision>,
}

pub fn main_head(repo: &Path, cfg: &TowerConfig) -> Result<String> {
    git::rev_parse(repo, &cfg.repo.main_branch).with_context(|| format!("resolve main branch {}", cfg.repo.main_branch))
}

pub fn scan_worktree(repo: &Path, wt: &git::WorktreeInfo, cfg: &TowerConfig) -> Result<WorktreeScan> {
    let wt_path = PathBuf::from(&wt.path);
    let mh = main_head(repo, cfg)?;
    let head = if wt.head.is_empty() { git::rev_parse(&wt_path, "HEAD")? } else { wt.head.clone() };
    let base = git::merge_base(&wt_path, &mh, &head).unwrap_or_else(|_| mh.clone());
    let files = git::changed_files(&wt_path, &base).unwrap_or_default();
    let mut items = Vec::new();
    let mut changed_names = Vec::new();
    for f in &files {
        let new_src = std::fs::read_to_string(wt_path.join(f)).ok();
        let base_src = git::file_at_ref(repo, &base, f);
        if let Some(ns) = &new_src { items.extend(index::extract_items(f, ns)); }
        changed_names.extend(index::changed_items(f, base_src.as_deref(), new_src.as_deref()));
    }
    changed_names.sort(); changed_names.dedup();
    let snapshot = git::snapshot_commit(&wt_path).unwrap_or_else(|_| head.clone());
    Ok(WorktreeScan { path: wt.path.clone(), head, snapshot, branch: wt.branch.clone(), base, files, items, changed_item_names: changed_names, status: "green".into(), reasons: vec![] })
}

pub fn landed_ids(store: &Store, repo_id: i64) -> std::collections::HashSet<String> {
    store.events_since(repo_id, 0).unwrap_or_default().into_iter().filter(|e| e.kind == "changeset_landed").filter_map(|e| serde_json::from_str::<serde_json::Value>(&e.payload_json).ok()).filter_map(|v| v.get("changeset_id").and_then(|x| x.as_str()).map(|s| s.to_string())).collect()
}

pub fn is_superseded(c: &crate::store::ChangesetRow) -> bool { c.superseded || c.status == "superseded" }

/// Worktrees whose changesets are *all* superseded: excluded from radar
/// pairing so a replaced record cannot produce phantom collisions (F4).
fn superseded_only_worktrees(store: &Store, repo: &Path) -> std::collections::HashSet<String> {
    let mut out = std::collections::HashSet::new();
    if let Ok(Some(r)) = store.repo_by_path(&git::canonical_str(repo)) {
        if let Ok(list) = store.list_changesets(r.id) {
            let mut by_wt: std::collections::HashMap<String, Vec<bool>> = std::collections::HashMap::new();
            for c in &list { by_wt.entry(c.worktree_path.clone()).or_default().push(is_superseded(c)); }
            for (wt, flags) in by_wt { if flags.iter().all(|f| *f) { out.insert(git::canonical_str(Path::new(&wt))); } }
        }
    }
    out
}

pub fn radar_scan(store: &Store, repo: &Path, cfg: &TowerConfig) -> Result<RadarResult> {
    let wts = git::worktree_list(repo)?;
    let mh = main_head(repo, cfg)?;
    // Tower's own rehearsal/landing scratch worktrees are not peer work:
    // scanning them would double-report every conflict already integrated.
    let canon_repo = crate::git::canonical_str(repo);
    let scratch = crate::git::canonical_str(&scratch_dir(store, &canon_repo));
    let landing = crate::git::canonical_str(&landing_scratch_dir(store, &canon_repo));
    let superseded_only = superseded_only_worktrees(store, repo);
    let mut scans = Vec::new();
    for wt in &wts {
        if wt.bare { continue; }
        let canon = crate::git::canonical_str(Path::new(&wt.path));
        if canon == scratch || canon == landing { continue; }
        match scan_worktree(repo, wt, cfg) { Ok(s) => scans.push(s), Err(e) => eprintln!("tower: scan {} failed: {e}", wt.path) }
    }
    let mut excluded: std::collections::HashSet<usize> = scans.iter().enumerate().filter(|(_, s)| superseded_only.contains(&crate::git::canonical_str(Path::new(&s.path)))).map(|(i, _)| i).collect();
    let mut reconciliation_suspended = std::collections::HashSet::new();
    if let Some(registered) = store.repo_by_path(&canon_repo)? {
        let assignments = store.list_assignments(registered.id)?;
        for (i, scan) in scans.iter().enumerate() {
            for assignment in assignments.iter().filter(|a| git::canonical_str(Path::new(&a.worktree_path)) == git::canonical_str(Path::new(&scan.path))) {
                let snapshot = git::snapshot_commit(Path::new(&scan.path))?;
                if crate::reconcile::changeset_suspended(store, registered.id, &assignment.id, &snapshot)? {
                    excluded.insert(i); reconciliation_suspended.insert(i); break;
                }
            }
        }
    }
    // Pairwise + vs main (skip main worktree itself when snapshot == main head and no files)
    let mut collisions = Vec::new();
    let mut red_map: std::collections::HashMap<usize, Vec<String>> = std::collections::HashMap::new();
    for i in 0..scans.len() {
        if excluded.contains(&i) { continue; }
        if scans[i].snapshot == mh && scans[i].files.is_empty() { continue; }
        let sim = git::merge_sim(repo, &scans[i].base, &mh, &scans[i].snapshot);
        if !sim.clean {
            red_map.entry(i).or_default().push(format!("conflicts with {} in {}", cfg.repo.main_branch, sim.conflict_files.join(",")));
            collisions.push(Collision { a: scans[i].path.clone(), b: format!("branch:{}", cfg.repo.main_branch), files: sim.conflict_files });
        }
    }
    for i in 0..scans.len() {
        for j in (i + 1)..scans.len() {
            if excluded.contains(&i) || excluded.contains(&j) { continue; }
            if scans[i].files.is_empty() || scans[j].files.is_empty() { continue; }
            if scans[i].snapshot == mh || scans[j].snapshot == mh { continue; }
            let base = git::merge_base(repo, &scans[i].snapshot, &scans[j].snapshot).unwrap_or_else(|_| scans[i].base.clone());
            let sim = git::merge_sim(repo, &base, &scans[i].snapshot, &scans[j].snapshot);
            if !sim.clean {
                collisions.push(Collision { a: scans[i].path.clone(), b: scans[j].path.clone(), files: sim.conflict_files.clone() });
                red_map.entry(i).or_default().push(format!("conflicts with {} in {}", scans[j].path, sim.conflict_files.join(",")));
                red_map.entry(j).or_default().push(format!("conflicts with {} in {}", scans[i].path, sim.conflict_files.join(",")));
            }
        }
    }
    for (i, s) in scans.iter_mut().enumerate() {
        if reconciliation_suspended.contains(&i) { s.status = "yellow".into(); s.reasons = vec!["shared reconciliation pending — excluded from radar pairing until the verified result is merged unchanged".into()]; continue; }
        if excluded.contains(&i) { s.status = "green".into(); s.reasons = vec!["superseded changeset worktree — excluded from radar pairing".into()]; continue; }
        if let Some(reasons) = red_map.get(&i) { s.status = "red".into(); s.reasons = reasons.clone(); }
        else if s.files.is_empty() { s.status = "green".into(); s.reasons = vec!["no changes vs merge-base".into()]; }
        else { s.status = "green".into(); s.reasons = vec!["merges cleanly (Tier-0)".into()]; }
    }
    Ok(RadarResult { worktrees: scans, collisions })
}

pub fn label_for(store: &Store, repo_id: i64, wt_path: &str) -> String {
    let canonical = git::canonical_str(Path::new(wt_path));
    if let Ok(list) = store.list_changesets(repo_id) {
        // latest non-superseded changeset for this worktree
        if let Some(c) = list.iter().rev().find(|c| git::canonical_str(Path::new(&c.worktree_path)) == canonical && !is_superseded(c)) { return c.id.clone(); }
    }
    if let Ok(list) = store.list_assignments(repo_id) {
        if let Some(a) = list.iter().find(|a| git::canonical_str(Path::new(&a.worktree_path)) == canonical && a.state == "open") { return a.id.clone(); }
    }
    wt_path.to_string()
}

/// Claimed-vs-actual for one changeset. `changed_items` are (file, item)
/// pairs so coverage is file-scoped: an item is inside-claim only when the
/// assignment holds that item's claim in that file, or a whole-file claim
/// on that file. `outside_items` entries are `{"file", "item"}` objects,
/// matching `outside_files`. A file with no claims at all is reported once
/// as a whole outside file; its items are not repeated in `outside_items`.
pub fn claimed_vs_actual(claimed: &[ClaimNode], files: &[String], changed_items: &[(String, String)]) -> serde_json::Value {
    let mut inside = Vec::new();
    let mut outside = Vec::new();
    let mut unclaimed = Vec::new();
    for f in files {
        // No claims held at all: nothing was promised, so nothing is "outside".
        // The files are still reported, as unclaimed (radar-mode changesets).
        if claimed.is_empty() { unclaimed.push(serde_json::json!({"file": f})); continue; }
        let file_claimed = claimed.iter().any(|c| &c.file == f);
        if !file_claimed { outside.push(serde_json::json!({"file": f, "item": serde_json::Value::Null})); continue; }
        // File- or item-claimed: the file itself is inside; any unclaimed
        // items changed inside it are reported in outside_items below.
        inside.push(serde_json::json!({"file": f}));
    }
    // Item-level outside check, scoped to files that carry claims.
    let mut item_outside = Vec::new();
    for (file, it) in changed_items {
        if !claimed.iter().any(|c| &c.file == file) { continue; }
        let short = it.rsplit("::").next().unwrap_or(it);
        let short = short.rsplit('.').next().unwrap_or(short);
        let covered = claimed.iter().any(|c| &c.file == file && (c.item.is_none() || c.item.as_deref() == Some(it.as_str()) || c.item.as_deref() == Some(short)));
        if !covered { item_outside.push(serde_json::json!({"file": file, "item": it})); }
    }
    serde_json::json!({"claimed": claimed, "inside": inside, "outside_files": outside, "outside_items": item_outside, "unclaimed_files": unclaimed})
}

/// Human-readable outside-claim detail from a claimed-vs-actual value:
/// outside items as `file#item`, outside files as `file (whole file)`.
/// Tolerates pre-2.2 stored values whose `outside_items` were bare names.
pub fn outside_detail(cva: &serde_json::Value) -> Vec<String> {
    let mut out = Vec::new();
    if let Some(items) = cva.get("outside_items").and_then(|v| v.as_array()) {
        for e in items {
            match (e.get("file").and_then(|v| v.as_str()), e.get("item").and_then(|v| v.as_str()), e.as_str()) {
                (Some(f), Some(i), _) => out.push(format!("{f}#{i}")),
                (_, Some(i), _) => out.push(i.to_string()),
                (_, _, Some(s)) => out.push(s.to_string()),
                _ => {}
            }
        }
    }
    if let Some(files) = cva.get("outside_files").and_then(|v| v.as_array()) {
        for e in files {
            match (e.get("file").and_then(|v| v.as_str()), e.get("item").and_then(|v| v.as_str()), e.as_str()) {
                (Some(f), Some(i), _) => out.push(format!("{f}#{i}")),
                (Some(f), None, _) => out.push(format!("{f} (whole file)")),
                (_, _, Some(s)) => out.push(format!("{s} (whole file)")),
                _ => {}
            }
        }
    }
    out
}

/// The outside-claim status reason, naming every outside item/file so the
/// hint it sits next to ("refresh claims or changeset") says what to claim.
pub fn outside_reason(cva: &serde_json::Value) -> String {
    let detail = outside_detail(cva);
    if detail.is_empty() { "touched files outside claim".to_string() } else { format!("touched files outside claim: {}", detail.join(", ")) }
}

pub fn outside_claim(cva: &serde_json::Value) -> bool {
    let f = cva.get("outside_files").and_then(|v| v.as_array()).map(|a| !a.is_empty()).unwrap_or(false);
    let i = cva.get("outside_items").and_then(|v| v.as_array()).map(|a| !a.is_empty()).unwrap_or(false);
    f || i
}

/// Recompute a worktree's diff payload (files, items JSON, changed
/// (file, item) pairs) against `base` — the "actual" side of
/// claimed-vs-actual, live.
pub fn live_diff(repo: &Path, wt: &Path, base: &str) -> Result<(Vec<String>, Vec<serde_json::Value>, Vec<(String, String)>)> {
    let files = git::changed_files(wt, base)?;
    let mut items_json = Vec::new();
    let mut changed_items = Vec::new();
    for f in &files {
        let new_src = std::fs::read_to_string(wt.join(f)).ok();
        let base_src = git::file_at_ref(repo, base, f);
        if let Some(ns) = &new_src { for it in index::extract_items(f, ns) { items_json.push(serde_json::to_value(it).unwrap()); } }
        for name in index::changed_items(f, base_src.as_deref(), new_src.as_deref()) { changed_items.push((f.clone(), name)); }
    }
    changed_items.sort(); changed_items.dedup();
    Ok((files, items_json, changed_items))
}

pub fn participants_value(pairs: &[(String, String)]) -> serde_json::Value {
    let mut map = serde_json::Map::new();
    for (k, v) in pairs { map.insert(k.clone(), serde_json::json!(v)); }
    serde_json::Value::Object(map)
}

/// A recorded gate pass still counts only while its proof context is intact
/// (F6): recorded main head is current and every recorded participant's
/// current snapshot is unchanged. Legacy rows without proof fields (v1
/// homes) are grandfathered as current.
pub fn gate_proof_current(g: &crate::store::GateRunRow, snapshots: &std::collections::HashMap<String, String>, current_main: &str) -> bool {
    if let Some(mh) = &g.main_head { if mh != current_main { return false; } }
    if let Some(pj) = &g.participants_json {
        let Ok(v) = serde_json::from_str::<serde_json::Value>(pj) else { return false };
        let Some(obj) = v.as_object() else { return false };
        for (pid, snap) in obj {
            let Some(recorded) = snap.as_str() else { return false };
            match snapshots.get(pid) { Some(cur) if cur == recorded => {}, _ => return false }
        }
    }
    true
}

pub const STALE_REASON: &str = "gate proof stale (source, peer, or main changed) — rerun gates";


pub fn scratch_dir(store: &Store, repo_path: &str) -> PathBuf {
    store.home.join("worktrees").join(crate::config::repo_hash(repo_path)).join("rehearsal")
}

/// Landing scratch: Tower-owned, separate from the rehearsal scratch (F5).
pub fn landing_scratch_dir(store: &Store, repo_path: &str) -> PathBuf {
    store.home.join("worktrees").join(crate::config::repo_hash(repo_path)).join("landing")
}

fn ensure_scratch_at(dir: PathBuf, repo: &Path, head: &str, ref_name: &str) -> Result<PathBuf> {
    if dir.join(".git").exists() {
        let _ = git::run_git_raw(&dir, &["merge", "--abort"]);
        let _ = git::run_git_raw(&dir, &["reset", "--hard", head]);
        let _ = git::run_git_raw(&dir, &["clean", "-fd"]);
    } else {
        if let Some(parent) = dir.parent() { std::fs::create_dir_all(parent)?; }
        let (code, _, err) = git::run_git_raw(repo, &["worktree", "add", "--detach", &dir.to_string_lossy(), head]);
        if code != 0 { bail!("create scratch worktree failed: {}", err.trim()); }
    }
    let cur = git::rev_parse(&dir, "HEAD").unwrap_or_else(|_| head.to_string());
    let _ = git::run_git_raw(repo, &["update-ref", ref_name, &cur]);
    Ok(dir)
}

pub fn ensure_landing_scratch(store: &Store, repo: &Path, based_main: &str) -> Result<PathBuf> {
    let repo_str = git::canonical_str(repo);
    ensure_scratch_at(landing_scratch_dir(store, &repo_str), repo, based_main, "refs/tower/landing-candidate")
}

pub fn ensure_scratch(store: &Store, repo: &Path, cfg: &TowerConfig) -> Result<PathBuf> {
    let repo_str = git::canonical_str(repo);
    let dir = scratch_dir(store, &repo_str);
    let mh = main_head(repo, cfg)?;
    if dir.join(".git").exists() {
        // reset tower-owned scratch only
        let _ = git::run_git_raw(&dir, &["reset", "--hard", &mh]);
        let _ = git::run_git_raw(&dir, &["clean", "-fd"]);
    } else {
        if let Some(parent) = dir.parent() { std::fs::create_dir_all(parent)?; }
        let (code, _, err) = git::run_git_raw(repo, &["worktree", "add", "--detach", &dir.to_string_lossy(), &mh]);
        if code != 0 { bail!("create scratch worktree failed: {}", err.trim()); }
    }
    let cur = git::rev_parse(&dir, "HEAD").unwrap_or(mh);
    let _ = git::run_git_raw(repo, &["update-ref", "refs/tower/integration", &cur]);
    Ok(dir)
}

pub fn integration_order(store: &Store, repo_id: i64) -> Result<Vec<serde_json::Value>> {
    let landed = landed_ids(store, repo_id);
    let mut out = Vec::new();
    for c in store.list_changesets(repo_id)? {
        if landed.contains(&c.id) || is_superseded(&c) { continue; }
        let wt = Path::new(&c.worktree_path);
        let snapshot = if wt.exists() { git::snapshot_commit(wt)? } else { c.head_commit.clone() };
        if crate::reconcile::changeset_suspended(store, repo_id, &c.assignment_id, &snapshot)? { continue; }
        out.push(serde_json::json!({"changeset_id": c.id, "task_id": c.task_id, "assignment_id": c.assignment_id, "reason": "creation order (no declared dependencies)"}));
    }
    Ok(out)
}

pub fn rehearse(store: &Store, repo_id: i64, repo: &Path, cfg: &TowerConfig, tiers: &[i64]) -> Result<serde_json::Value> {
    let key = format!("rehearse:{tiers:?}");
    let mut reservation = match resources::acquire(store, repo_id, &key, &resources::resource_names(repo_id, cfg), serde_json::json!({"operation": "rehearse", "tiers": tiers}))? {
        Reservation::Acquired(guard) => guard,
        Reservation::Queued(job) => return Ok(resources::queued_receipt(&job)),
    };
    let mh = main_head(repo, cfg)?;
    let scratch = ensure_scratch(store, repo, cfg)?;
    store.emit(repo_id, "rehearsal_started", serde_json::json!({"scratch": scratch.to_string_lossy()}))?;
    let order = integration_order(store, repo_id)?;
    let mut results = Vec::new();
    let mut merged_ids: Vec<String> = Vec::new();
    let mut participants: Vec<(String, String)> = Vec::new();
    for entry in &order {
        let cid = entry["changeset_id"].as_str().unwrap().to_string();
        let cs = match store.get_changeset(repo_id, &cid)? { Some(c) => c, None => continue };
        // Use committed head; if worktree has uncommitted changes snapshot them read-only.
        let wt = PathBuf::from(&cs.worktree_path);
        let head = if wt.exists() { git::snapshot_commit(&wt).unwrap_or_else(|_| cs.head_commit.clone()) } else { cs.head_commit.clone() };
        let (code, out, err) = git::run_git_raw(&scratch, &["-c", "user.name=Tower", "-c", "user.email=tower@local", "merge", "--no-edit", &head]);
        if code != 0 {
            let msg = format!("rehearsal merge conflict: {} {}", out.trim(), err.trim());
            let _ = git::run_git_raw(&scratch, &["merge", "--abort"]);
            store.update_changeset_status(repo_id, &cid, "red", &serde_json::json!([msg.clone()]))?;
            results.push(serde_json::json!({"changeset_id": cid, "merged": false, "error": msg}));
            continue;
        }
        merged_ids.push(cid.clone());
        participants.push((cid.clone(), head.clone()));
        results.push(serde_json::json!({"changeset_id": cid, "merged": true}));
    }
    // Gates on rehearsal tree. Attribute the result only to changesets that
    // actually merged into this tree; a conflicted-out changeset is not
    // present in the tree and must not inherit its verdict.
    let tree_hash = git::rev_parse(&scratch, "HEAD^{tree}").unwrap_or_default();
    let participants_json = participants_value(&participants);
    let mut gate_results = Vec::new();
    for tier in tiers {
        let cmd = match tier { 1 => cfg.gates.check.clone(), 2 => cfg.gates.test.clone(), _ => None };
        let Some(cmd) = cmd else { gate_results.push(serde_json::json!({"tier": tier, "skipped": true, "reason": "not configured"})); continue; };
        let (passed, output) = reservation.run_gate(&scratch, &cmd, cfg.gates.timeout_secs)?;
        gate_results.push(serde_json::json!({"tier": tier, "command": cmd, "passed": passed, "resource_job_id": reservation.id, "output_tail": tail(&output, 2000), "attributed_to": merged_ids, "tree_hash": tree_hash, "main_head": mh, "participants": participants_json}));
        for cid in &merged_ids {
            store.record_gate_with_proof(repo_id, cid, *tier, &cmd, passed, &output, Some(&tree_hash), Some(&mh), Some(&participants_json))?;
        }
    }
    let new_head = git::rev_parse(&scratch, "HEAD").unwrap_or_default();
    if !new_head.is_empty() { let _ = git::run_git_raw(repo, &["update-ref", "refs/tower/integration", &new_head]); }
    // Refresh changeset statuses from simulation + gates
    refresh_changeset_statuses(store, repo_id, repo, cfg)?;
    store.emit(repo_id, "rehearsal_finished", serde_json::json!({"integration_head": new_head}))?;
    Ok(serde_json::json!({"scratch": scratch.to_string_lossy(), "resource_job_id": reservation.id, "integration_head": new_head, "order": order, "merges": results, "gates": gate_results}))
}

fn tail(s: &str, n: usize) -> String {
    let mut start = s.len().saturating_sub(n);
    while !s.is_char_boundary(start) { start += 1; }
    s[start..].to_string()
}

pub fn gate_run_single(store: &Store, repo_id: i64, repo: &Path, cfg: &TowerConfig, changeset_id: &str, tier: i64) -> Result<serde_json::Value> {
    if tier != 1 && tier != 2 { bail!("gate tier must be 1 or 2"); }
    let cs0 = store.get_changeset(repo_id, changeset_id)?.with_context(|| format!("unknown changeset {changeset_id}"))?;
    if is_superseded(&cs0) { bail!("changeset {changeset_id} is superseded; gates are not run for it"); }
    let key = format!("gate:{changeset_id}:{tier}");
    let mut reservation = match resources::acquire(store, repo_id, &key, &resources::resource_names(repo_id, cfg), serde_json::json!({"operation": "gate", "changeset_id": changeset_id, "tier": tier}))? {
        Reservation::Acquired(guard) => guard,
        Reservation::Queued(job) => return Ok(resources::queued_receipt(&job)),
    };
    let mh = main_head(repo, cfg)?;
    let scratch = ensure_scratch(store, repo, cfg)?;
    // rehearsal merges up to and including this changeset
    let order = integration_order(store, repo_id)?;
    let mut participants: Vec<(String, String)> = Vec::new();
    let mut target_in_order = false;
    for entry in &order {
        let cid = entry["changeset_id"].as_str().unwrap();
        let cs = store.get_changeset(repo_id, cid)?.unwrap();
        let wt = PathBuf::from(&cs.worktree_path);
        let head = if wt.exists() { git::snapshot_commit(&wt).unwrap_or_else(|_| cs.head_commit.clone()) } else { cs.head_commit.clone() };
        let (code, out, err) = git::run_git_raw(&scratch, &["-c", "user.name=Tower", "-c", "user.email=tower@local", "merge", "--no-edit", &head]);
        if code != 0 {
            let _ = git::run_git_raw(&scratch, &["merge", "--abort"]);
            if cid == changeset_id { bail!("rehearsal merge conflict for {cid}: {} {}", out.trim(), err.trim()); }
        } else {
            participants.push((cid.to_string(), head.clone()));
        }
        if cid == changeset_id { target_in_order = true; break; }
    }
    if !target_in_order {
        // Not in integration order (e.g. already landed): rehearse it alone.
        let wt = PathBuf::from(&cs0.worktree_path);
        let head = if wt.exists() { git::snapshot_commit(&wt).unwrap_or_else(|_| cs0.head_commit.clone()) } else { cs0.head_commit.clone() };
        let (code, out, err) = git::run_git_raw(&scratch, &["-c", "user.name=Tower", "-c", "user.email=tower@local", "merge", "--no-edit", &head]);
        if code != 0 { let _ = git::run_git_raw(&scratch, &["merge", "--abort"]); bail!("rehearsal merge conflict for {changeset_id}: {} {}", out.trim(), err.trim()); }
        participants.push((changeset_id.to_string(), head));
    }
    let cmd = match tier { 1 => cfg.gates.check.clone(), 2 => cfg.gates.test.clone(), _ => None };
    let Some(cmd) = cmd else { return Ok(serde_json::json!({"changeset_id": changeset_id, "tier": tier, "skipped": true, "reason": "tier not configured — Tier-0 simulation only"})); };
    let (passed, output) = reservation.run_gate(&scratch, &cmd, cfg.gates.timeout_secs)?;
    let tree_hash = git::rev_parse(&scratch, "HEAD^{tree}").unwrap_or_default();
    let participants_json = participants_value(&participants);
    store.record_gate_with_proof(repo_id, changeset_id, tier, &cmd, passed, &output, Some(&tree_hash), Some(&mh), Some(&participants_json))?;
    refresh_changeset_statuses(store, repo_id, repo, cfg)?;
    let cs = store.get_changeset(repo_id, changeset_id)?.unwrap();
    Ok(serde_json::json!({"changeset_id": changeset_id, "tier": tier, "command": cmd, "passed": passed, "resource_job_id": reservation.id, "output_tail": tail(&output, 2000), "status": cs.status, "reasons": serde_json::from_str::<serde_json::Value>(&cs.reasons_json).unwrap_or_default()}))
}

pub fn refresh_changeset_statuses(store: &Store, repo_id: i64, repo: &Path, cfg: &TowerConfig) -> Result<()> {
    let mh = main_head(repo, cfg)?;
    let landed = landed_ids(store, repo_id);
    let changesets = store.list_changesets(repo_id)?;
    // Active set, in integration (creation) order: landed and superseded are out.
    let mut active: Vec<&crate::store::ChangesetRow> = Vec::new();
    for c in &changesets {
        if landed.contains(&c.id) || is_superseded(c) { continue; }
        let wt = Path::new(&c.worktree_path);
        let snapshot = if wt.exists() { git::snapshot_commit(wt)? } else { c.head_commit.clone() };
        if crate::reconcile::changeset_suspended(store, repo_id, &c.assignment_id, &snapshot)? {
            store.update_changeset_status(repo_id, &c.id, "yellow", &serde_json::json!(["shared reconciliation pending — wait for the verified result, then merge it and validate the consumer"]))?;
            continue;
        }
        active.push(c);
    }
    let position: std::collections::HashMap<String, usize> = active.iter().enumerate().map(|(i, c)| (c.id.clone(), i)).collect();
    // Live snapshot, head, base, and claimed-vs-actual per changeset (F2:
    // CVA is recomputed from live claims and the live diff, not frozen).
    struct Live { snap: String, base: String, cva: serde_json::Value }
    let mut live: std::collections::HashMap<String, Live> = std::collections::HashMap::new();
    for c in &active {
        let wt = PathBuf::from(&c.worktree_path);
        let (snap, head, snapshot_proven) = if wt.exists() {
            let snapshot = git::snapshot_commit(&wt).ok();
            let proven = snapshot.is_some();
            (snapshot.unwrap_or_else(|| c.head_commit.clone()), git::rev_parse(&wt, "HEAD").unwrap_or_else(|_| c.head_commit.clone()), proven)
        } else { (c.head_commit.clone(), c.head_commit.clone(), true) };
        let base = git::merge_base(repo, &mh, &snap).unwrap_or_else(|_| c.base_commit.clone());
        let cva = if wt.exists() {
            match live_diff(repo, &wt, &base) {
                Ok((files, items_json, changed_items)) => {
                    let mut claimed = store.claims_for(repo_id, &c.assignment_id)?;
                    if snapshot_proven {
                        // Returned implementation coverage validates its exact source;
                        // it does not grant this assignment permission to edit it.
                        claimed.extend(crate::reconcile::returned_coverage(store, repo_id, &c.assignment_id, &snap)?);
                    }
                    let cva = claimed_vs_actual(&claimed, &files, &changed_items);
                    let files_v = serde_json::json!(files);
                    let items_v = serde_json::json!(items_json);
                    if files_v.to_string() != c.files_json || items_v.to_string() != c.items_json || cva.to_string() != c.claimed_vs_actual_json || head != c.head_commit || base != c.base_commit {
                        let _ = store.update_changeset_snapshot(repo_id, &c.id, &base, &head, &files_v, &items_v, &cva, false);
                    }
                    cva
                }
                Err(_) => serde_json::from_str(&c.claimed_vs_actual_json).unwrap_or_default(),
            }
        } else { serde_json::from_str(&c.claimed_vs_actual_json).unwrap_or_default() };
        live.insert(c.id.clone(), Live { snap, base, cva });
    }
    let snap_map: std::collections::HashMap<String, String> = live.iter().map(|(k, v)| (k.clone(), v.snap.clone())).collect();
    // Pairwise conflicts computed once; attribution is order-aware (F1).
    let mut pair_conflicts: std::collections::HashMap<(String, String), Vec<String>> = std::collections::HashMap::new();
    for i in 0..active.len() {
        for j in (i + 1)..active.len() {
            let (a, b) = (&active[i].id, &active[j].id);
            let (sa, sb) = (&live[a].snap, &live[b].snap);
            if sa == sb { continue; }
            let base = git::merge_base(repo, sa, sb).unwrap_or_else(|_| live[a].base.clone());
            let sim = git::merge_sim(repo, &base, sa, sb);
            if !sim.clean { pair_conflicts.insert((a.clone(), b.clone()), sim.conflict_files); }
        }
    }
    for c in &active {
        let my = &live[&c.id];
        let my_pos = position[&c.id];
        let mut red = Vec::new();
        let mut info = Vec::new();
        let sim_main = git::merge_sim(repo, &my.base, &mh, &my.snap);
        if !sim_main.clean { red.push(format!("conflicts with {} in {}", cfg.repo.main_branch, sim_main.conflict_files.join(","))); }
        for other in &active {
            if other.id == c.id { continue; }
            let key = if my_pos < position[&other.id] { (c.id.clone(), other.id.clone()) } else { (other.id.clone(), c.id.clone()) };
            let Some(files) = pair_conflicts.get(&key) else { continue };
            if position[&other.id] < my_pos {
                red.push(format!("conflicts with {} in {}", other.id, files.join(",")));
            } else {
                info.push(format!("blocks {} in {} (conflicts with later changeset; does not block this changeset)", other.id, files.join(",")));
            }
        }
        let gates = store.gate_runs_for(repo_id, &c.id)?;
        let latest = |tier: i64| gates.iter().rev().find(|g| g.tier == tier);
        let mut gate_fail: Option<String> = None;
        let mut stale = false;
        let mut pending: Vec<String> = Vec::new();
        for (tier, configured, label) in [(1i64, cfg.gates.check.is_some(), "Tier-1 check gate has not passed yet"), (2i64, cfg.gates.test.is_some(), "Tier-2 test gate has not passed yet")] {
            if !configured { continue; }
            match latest(tier) {
                Some(g) if !g.passed => { if gate_fail.is_none() { gate_fail = Some(format!("gate failed: {}", g.command)); } }
                Some(g) => { if !gate_proof_current(g, &snap_map, &mh) { stale = true; } }
                None => pending.push(label.to_string()),
            }
        }
        let outside = outside_claim(&my.cva);
        let (status, mut reasons) = if !red.is_empty() {
            ("red".to_string(), red)
        } else if let Some(f) = gate_fail {
            ("red".to_string(), vec![f])
        } else {
            let mut yellow = Vec::new();
            if outside { yellow.push(outside_reason(&my.cva)); }
            if stale { yellow.push(STALE_REASON.to_string()); }
            yellow.extend(pending);
            if !yellow.is_empty() {
                if outside && !stale { yellow.push("merges cleanly (Tier-0)".to_string()); }
                ("yellow".to_string(), yellow)
            } else if cfg.has_gates() {
                ("green".to_string(), vec!["merges cleanly and configured gates passed".to_string()])
            } else {
                ("green".to_string(), vec!["merges cleanly (Tier-0 simulation only)".to_string()])
            }
        };
        reasons.extend(info);
        if status != c.status || serde_json::json!(reasons).to_string() != c.reasons_json {
            store.update_changeset_status(repo_id, &c.id, &status, &serde_json::json!(reasons))?;
        }
    }
    Ok(())
}

fn reason_strings(reasons_json: &str) -> Vec<String> {
    serde_json::from_str::<serde_json::Value>(reasons_json).ok().and_then(|v| v.as_array().map(|a| a.iter().filter_map(|x| x.as_str().map(|s| s.to_string())).collect())).unwrap_or_default()
}

fn is_gate_proof_reason(r: &str) -> bool {
    r.contains("has not passed yet") || r.contains("gate proof stale") || r == "gates have not run yet"
}

/// Landable = green, or yellow whose only blockers are pending/stale gate
/// proofs (the candidate runs gates itself). Informational `blocks …`
/// reasons never block (F1).
pub fn changeset_landable(cs: &crate::store::ChangesetRow) -> bool {
    if is_superseded(cs) { return false; }
    if cs.status == "green" { return true; }
    if cs.status != "yellow" { return false; }
    let cva: serde_json::Value = serde_json::from_str(&cs.claimed_vs_actual_json).unwrap_or_default();
    if outside_claim(&cva) { return false; }
    let reasons = reason_strings(&cs.reasons_json);
    !reasons.is_empty() && reasons.iter().all(|r| is_gate_proof_reason(r) || r.starts_with("blocks ") || r.starts_with("merges cleanly"))
}

#[allow(dead_code)]
pub(crate) enum Candidate {
    Built { candidate_head: String, tree_hash: String, gate_results: Vec<serde_json::Value> },
    Conflict { files: Vec<String> },
    GateFailed { tier: i64, command: String, output: String, tree_hash: String },
}

/// Build (or rebuild) the landing candidate for one changeset in the
/// Tower-owned landing scratch and run gates on it (F5). Worker and main
/// worktrees are never touched here.
#[cfg(test)]
pub(crate) fn build_landing_candidate(store: &Store, repo_id: i64, repo: &Path, cfg: &TowerConfig, changeset_id: &str, source_head: &str, based_main: &str, record_gates: bool) -> Result<Candidate> {
    let key = format!("candidate:{changeset_id}");
    let mut reservation = match resources::acquire(store, repo_id, &key, &resources::resource_names(repo_id, cfg), serde_json::json!({"operation": "candidate", "changeset_id": changeset_id}))? {
        Reservation::Acquired(guard) => guard,
        Reservation::Queued(job) => bail!("landing candidate queued: {}", resources::queued_receipt(&job)),
    };
    build_landing_candidate_reserved(store, repo_id, repo, cfg, changeset_id, source_head, based_main, record_gates, &mut reservation)
}

fn build_landing_candidate_reserved(store: &Store, repo_id: i64, repo: &Path, cfg: &TowerConfig, changeset_id: &str, source_head: &str, based_main: &str, record_gates: bool, reservation: &mut ResourceGuard<'_>) -> Result<Candidate> {
    let scratch = ensure_landing_scratch(store, repo, based_main)?;
    let (code, out, err) = git::run_git_raw(&scratch, &["-c", "user.name=Tower", "-c", "user.email=tower@local", "merge", "--no-edit", source_head]);
    if code != 0 {
        let (_, unmerged, _) = git::run_git_raw(&scratch, &["diff", "--name-only", "--diff-filter=U"]);
        let mut files: Vec<String> = unmerged.lines().map(|l| l.trim().to_string()).filter(|l| !l.is_empty()).collect();
        if files.is_empty() {
            let (_, lst, _) = git::run_git_raw(&scratch, &["ls-files", "-u"]);
            for l in lst.lines() { if let Some(p) = l.split('\t').nth(1) { if !files.contains(&p.to_string()) { files.push(p.to_string()); } } }
        }
        let _ = git::run_git_raw(&scratch, &["merge", "--abort"]);
        let _ = (out, err);
        return Ok(Candidate::Conflict { files });
    }
    let candidate_head = git::rev_parse(&scratch, "HEAD")?;
    let tree_hash = git::rev_parse(&scratch, "HEAD^{tree}").unwrap_or_default();
    let _ = git::run_git_raw(repo, &["update-ref", "refs/tower/landing-candidate", &candidate_head]);
    let participants = participants_value(&[(changeset_id.to_string(), source_head.to_string())]);
    let mut gate_results = Vec::new();
    for (tier, cmd) in [(1i64, cfg.gates.check.clone()), (2i64, cfg.gates.test.clone())] {
        let Some(cmd) = cmd else { continue };
        let (passed, output) = reservation.run_gate(&scratch, &cmd, cfg.gates.timeout_secs)?;
        gate_results.push(serde_json::json!({"tier": tier, "command": cmd, "passed": passed, "resource_job_id": reservation.id, "output_tail": tail(&output, 2000), "tree_hash": tree_hash, "main_head": based_main, "participants": participants}));
        if record_gates { store.record_gate_with_proof(repo_id, changeset_id, tier, &cmd, passed, &output, Some(&tree_hash), Some(based_main), Some(&participants))?; }
        if !passed { return Ok(Candidate::GateFailed { tier, command: cmd, output, tree_hash }); }
    }
    Ok(Candidate::Built { candidate_head, tree_hash, gate_results })
}

fn main_worktree(repo: &Path, main_br: &str) -> Result<Option<PathBuf>> {
    Ok(git::worktree_list(repo)?.iter().find(|w| w.branch.as_deref() == Some(main_br)).map(|w| PathBuf::from(&w.path)))
}

/// Advance the main ref to `new_head` (which descends from `expected_old`):
/// `--ff-only` in the checked-out main worktree when there is one (it must
/// be clean and still at the expected head), else a compare-and-swap
/// `update-ref`. Returns Err if main moved or is dirty.
fn advance_main(repo: &Path, cfg: &TowerConfig, expected_old: &str, new_head: &str) -> Result<()> {
    let main_br = &cfg.repo.main_branch;
    if let Some(mpath) = main_worktree(repo, main_br)? {
        if git::is_dirty(&mpath) { bail!("main worktree {} is dirty; refusing to land", mpath.display()); }
        let cur = git::rev_parse(&mpath, "HEAD")?;
        if cur != expected_old { bail!("main moved: expected {expected_old}, found {cur}"); }
        let (code, out, err) = git::run_git_raw(&mpath, &["merge", "--ff-only", new_head]);
        if code != 0 { bail!("advancing main failed: {} {}", out.trim(), err.trim()); }
    } else {
        let (code, out, err) = git::run_git_raw(repo, &["update-ref", &format!("refs/heads/{main_br}"), new_head, expected_old]);
        if code != 0 { bail!("advancing main (compare-and-swap) failed — main may have moved: {} {}", out.trim(), err.trim()); }
    }
    Ok(())
}

fn committed_source_head(cs: &crate::store::ChangesetRow) -> Result<String> {
    let wt = PathBuf::from(&cs.worktree_path);
    if wt.exists() {
        if git::is_dirty(&wt) { bail!("worktree {} has uncommitted changes; commit them before landing", cs.worktree_path); }
        Ok(git::rev_parse(&wt, "HEAD")?)
    } else { Ok(cs.head_commit.clone()) }
}

fn finalize_landing(store: &Store, repo_id: i64, repo: &Path, cfg: &TowerConfig, cs: &crate::store::ChangesetRow, source_head: &str, candidate_head: &str, based_main: &str, method: &str) -> Result<serde_json::Value> {
    let main_br = cfg.repo.main_branch.clone();
    // Claims belong to the assignment, not the changeset (Phase 2.2 Fix A):
    // landing never releases them. They go away only on explicit
    // `claim release` or on `assignment close`. The claims still held are
    // named in the landed event and the landing receipt so the state after
    // a landing is explicit, never silently empty.
    let claims_held = store.claims_for(repo_id, &cs.assignment_id).unwrap_or_default();
    store.emit(repo_id, "changeset_landed", serde_json::json!({"changeset_id": cs.id, "task_id": cs.task_id, "assignment_id": cs.assignment_id, "head": candidate_head, "source_head": source_head, "candidate_head": candidate_head, "based_on_main_head": based_main, "main_branch": main_br, "method": method, "claims_held": claims_held}))?;
    store.set_task_state(repo_id, &cs.task_id, "landed")?;
    // Landing supersedes other unlanded changesets of the same task (F4).
    let landed_now = landed_ids(store, repo_id);
    for other in store.list_changesets(repo_id)? {
        if other.task_id == cs.task_id && other.id != cs.id && !landed_now.contains(&other.id) && !is_superseded(&other) {
            store.mark_superseded(repo_id, &other.id, Some(&cs.id))?;
        }
    }
    let _ = refresh_changeset_statuses(store, repo_id, repo, cfg);
    Ok(serde_json::json!({"changeset_id": cs.id, "landed": true, "head": candidate_head, "source_head": source_head, "candidate_head": candidate_head, "based_on_main_head": based_main, "main_branch": main_br, "method": method, "claims_held": claims_held}))
}

pub fn land_changeset(store: &Store, repo_id: i64, repo: &Path, cfg: &TowerConfig, changeset_id: &str) -> Result<serde_json::Value> {
    store.get_changeset(repo_id, changeset_id)?.with_context(|| format!("unknown changeset {changeset_id}"))?;
    let key = format!("land:{changeset_id}");
    let mut reservation = match resources::acquire(store, repo_id, &key, &resources::resource_names(repo_id, cfg), serde_json::json!({"operation": "land", "changeset_id": changeset_id}))? {
        Reservation::Acquired(guard) => guard,
        Reservation::Queued(job) => return Ok(resources::queued_receipt(&job)),
    };
    refresh_changeset_statuses(store, repo_id, repo, cfg)?;
    let cs = store.get_changeset(repo_id, changeset_id)?.with_context(|| format!("unknown changeset {changeset_id}"))?;
    crate::reconcile::ensure_no_unresolved_stubs(store, repo_id, &cs.assignment_id)?;
    if landed_ids(store, repo_id).contains(&cs.id) { bail!("changeset {} has already landed", cs.id); }
    if !changeset_landable(&cs) {
        bail!("refusing to land non-green changeset {} (status={}, not landable, reasons={})", cs.id, cs.status, cs.reasons_json);
    }
    let main_br = cfg.repo.main_branch.clone();
    // Existing refusal kept: a dirty checked-out main worktree blocks landing.
    if let Some(mpath) = main_worktree(repo, &main_br)? {
        if git::is_dirty(&mpath) { bail!("main worktree {} is dirty; refusing to land", mpath.display()); }
    }
    let source_head = committed_source_head(&cs)?;
    let mh = main_head(repo, cfg)?;
    let mb = git::merge_base(repo, &mh, &source_head)?;
    if cs.status == "green" && mb == mh {
        // Fast-forward case: unchanged mechanics (green already gate-proven).
        advance_main(repo, cfg, &mh, &source_head)?;
        return finalize_landing(store, repo_id, repo, cfg, &cs, &source_head, &source_head, &mh, "fast-forward");
    }
    // Tower-owned landing candidate (F5): build on current main, gate it,
    // re-check that neither head moved, advance main only to it. Rebuild
    // once if a head moved; fail asking for a retry if it moves again.
    for attempt in 0..2 {
        let based_main = main_head(repo, cfg)?;
        let src = committed_source_head(&cs)?;
        match build_landing_candidate_reserved(store, repo_id, repo, cfg, &cs.id, &src, &based_main, true, &mut reservation)? {
            Candidate::Conflict { files } => {
                let files_s = if files.is_empty() { "(unmerged files unavailable)".to_string() } else { files.join(",") };
                store.update_changeset_status(repo_id, &cs.id, "red", &serde_json::json!([format!("conflicts with {} in {}", main_br, files_s)]))?;
                bail!("landing candidate for {} conflicts with {} in {}; main unchanged, worker worktree untouched", cs.id, main_br, files_s);
            }
            Candidate::GateFailed { tier, command, output, .. } => {
                store.update_changeset_status(repo_id, &cs.id, "red", &serde_json::json!([format!("gate failed: {command}")]))?;
                bail!("landing candidate for {} failed Tier-{tier} gate ({command}): {}", cs.id, tail(&output, 2000));
            }
            Candidate::Built { candidate_head, .. } => {
                let now_main = main_head(repo, cfg)?;
                let now_src = committed_source_head(&cs)?;
                if now_main != based_main || now_src != src {
                    if attempt == 0 { continue; }
                    bail!("main or source head moved while building the landing candidate for {}; please retry landing", cs.id);
                }
                match advance_main(repo, cfg, &based_main, &candidate_head) {
                    Ok(()) => return finalize_landing(store, repo_id, repo, cfg, &cs, &src, &candidate_head, &based_main, "candidate"),
                    Err(e) => {
                        // A moved main between re-check and advance counts as a move.
                        let now_main = main_head(repo, cfg).unwrap_or_default();
                        if now_main != based_main && attempt == 0 { continue; }
                        if now_main != based_main { bail!("main or source head moved while building the landing candidate for {}; please retry landing", cs.id); }
                        return Err(e);
                    }
                }
            }
        }
    }
    bail!("main or source head moved while building the landing candidate for {}; please retry landing", cs.id)
}

pub fn register_changeset(store: &Store, repo_id: i64, repo: &Path, cfg: &TowerConfig, assignment_id: &str, worktree_path: &str) -> Result<serde_json::Value> {
    let asg = store.get_assignment(repo_id, assignment_id)?.with_context(|| format!("unknown assignment {assignment_id}"))?;
    let wt = PathBuf::from(worktree_path);
    let mh = main_head(repo, cfg)?;
    let head = crate::git::rev_parse(&wt, "HEAD")?;
    let base = crate::git::merge_base(&wt, &mh, &head).unwrap_or(mh);
    let (files, items_json, changed_items) = live_diff(repo, &wt, &base)?;
    let snapshot = git::snapshot_commit(&wt)?;
    let mut claimed = store.claims_for(repo_id, assignment_id)?;
    claimed.extend(crate::reconcile::returned_coverage(store, repo_id, assignment_id, &snapshot)?);
    let cva = claimed_vs_actual(&claimed, &files, &changed_items);
    let outside = outside_claim(&cva);
    let (status, reasons) = if outside { ("yellow", vec![outside_reason(&cva)]) }
        else if cfg.has_gates() { ("yellow", vec!["gates have not run yet".to_string()]) }
        else { ("green", vec!["registered (Tier-0 simulation pending refresh)".to_string()]) };
    let landed = landed_ids(store, repo_id);
    let cs = store.create_changeset(repo_id, assignment_id, &asg.task_id, worktree_path, &asg.branch, &base, &head, &serde_json::json!(files), &serde_json::json!(items_json), &cva, status, &serde_json::json!(reasons))?;
    // Re-registering an assignment supersedes its previous unlanded changesets (F4).
    for old in store.list_changesets(repo_id)? {
        if old.assignment_id == assignment_id && old.id != cs.id && !landed.contains(&old.id) && !is_superseded(&old) {
            store.mark_superseded(repo_id, &old.id, Some(&cs.id))?;
        }
    }
    store.set_task_state(repo_id, &asg.task_id, "integrating")?;
    refresh_changeset_statuses(store, repo_id, repo, cfg)?;
    let cs2 = store.get_changeset(repo_id, &cs.id)?.unwrap();
    Ok(serde_json::json!({"changeset_id": cs2.id, "task_id": cs2.task_id, "assignment_id": cs2.assignment_id, "status": cs2.status, "version": cs2.version, "reasons": serde_json::from_str::<serde_json::Value>(&cs2.reasons_json).unwrap_or_default(), "files": serde_json::from_str::<serde_json::Value>(&cs2.files_json).unwrap_or_default(), "claimed_vs_actual": serde_json::from_str::<serde_json::Value>(&cs2.claimed_vs_actual_json).unwrap_or_default()}))
}

/// Explicit refresh (F2): recompute live files/items/CVA/head, bump the
/// changeset version, and re-derive its status.
pub fn refresh_changeset(store: &Store, repo_id: i64, repo: &Path, cfg: &TowerConfig, changeset_id: &str) -> Result<serde_json::Value> {
    let cs = store.get_changeset(repo_id, changeset_id)?.with_context(|| format!("unknown changeset {changeset_id}"))?;
    if is_superseded(&cs) { bail!("changeset {changeset_id} is superseded and cannot be refreshed"); }
    if landed_ids(store, repo_id).contains(changeset_id) { bail!("changeset {changeset_id} has landed and cannot be refreshed"); }
    let wt = PathBuf::from(&cs.worktree_path);
    let mh = main_head(repo, cfg)?;
    let head = if wt.exists() { git::rev_parse(&wt, "HEAD").unwrap_or_else(|_| cs.head_commit.clone()) } else { cs.head_commit.clone() };
    let base = git::merge_base(repo, &mh, &head).unwrap_or_else(|_| cs.base_commit.clone());
    let (files_v, items_v, cva) = if wt.exists() {
        let (files, items_json, changed_items) = live_diff(repo, &wt, &base)?;
        let snapshot = git::snapshot_commit(&wt)?;
        let mut claimed = store.claims_for(repo_id, &cs.assignment_id)?;
        claimed.extend(crate::reconcile::returned_coverage(store, repo_id, &cs.assignment_id, &snapshot)?);
        let cva = claimed_vs_actual(&claimed, &files, &changed_items);
        (serde_json::json!(files), serde_json::json!(items_json), cva)
    } else {
        (serde_json::from_str(&cs.files_json).unwrap_or_default(), serde_json::from_str(&cs.items_json).unwrap_or_default(), serde_json::from_str(&cs.claimed_vs_actual_json).unwrap_or_default())
    };
    let version = store.update_changeset_snapshot(repo_id, changeset_id, &base, &head, &files_v, &items_v, &cva, true)?;
    store.emit(repo_id, "changeset_refreshed", serde_json::json!({"changeset_id": changeset_id, "version": version, "head": head}))?;
    refresh_changeset_statuses(store, repo_id, repo, cfg)?;
    let cs2 = store.get_changeset(repo_id, changeset_id)?.unwrap();
    Ok(serde_json::json!({"changeset_id": cs2.id, "task_id": cs2.task_id, "assignment_id": cs2.assignment_id, "status": cs2.status, "version": cs2.version, "head": cs2.head_commit, "reasons": serde_json::from_str::<serde_json::Value>(&cs2.reasons_json).unwrap_or_default(), "files": serde_json::from_str::<serde_json::Value>(&cs2.files_json).unwrap_or_default(), "claimed_vs_actual": serde_json::from_str::<serde_json::Value>(&cs2.claimed_vs_actual_json).unwrap_or_default()}))
}

pub fn supersede_changeset(store: &Store, repo_id: i64, repo: &Path, cfg: &TowerConfig, changeset_id: &str) -> Result<serde_json::Value> {
    let cs = store.get_changeset(repo_id, changeset_id)?.with_context(|| format!("unknown changeset {changeset_id}"))?;
    if landed_ids(store, repo_id).contains(changeset_id) { bail!("changeset {changeset_id} has landed and cannot be superseded"); }
    if !is_superseded(&cs) {
        store.mark_superseded(repo_id, changeset_id, None)?;
        let _ = refresh_changeset_statuses(store, repo_id, repo, cfg);
    }
    let cs2 = store.get_changeset(repo_id, changeset_id)?.unwrap();
    Ok(serde_json::json!({"changeset_id": cs2.id, "superseded": true, "status": cs2.status, "superseded_by": cs2.superseded_by}))
}

fn live_snapshot(cs: &crate::store::ChangesetRow) -> String {
    let wt = PathBuf::from(&cs.worktree_path);
    if wt.exists() { git::snapshot_commit(&wt).unwrap_or_else(|_| cs.head_commit.clone()) } else { cs.head_commit.clone() }
}

/// Conflict detail for one changeset (Phase 2.2 Fix D): for every file it
/// conflicts in against main or an *earlier* unlanded changeset — the same
/// set that makes its status red in `refresh_changeset_statuses` — the
/// conflicting hunk headers from `git merge-tree`. Read-only: nothing is
/// merged, no worktree is written, no state changes beyond the status
/// refresh that keeps the answer in step with `changeset status`.
/// Landed and superseded changesets report an empty conflict list.
pub fn changeset_conflicts(store: &Store, repo_id: i64, repo: &Path, cfg: &TowerConfig, changeset_id: &str) -> Result<serde_json::Value> {
    store.get_changeset(repo_id, changeset_id)?.with_context(|| format!("unknown changeset {changeset_id}"))?;
    refresh_changeset_statuses(store, repo_id, repo, cfg)?;
    let cs = store.get_changeset(repo_id, changeset_id)?.with_context(|| format!("unknown changeset {changeset_id}"))?;
    let empty = serde_json::json!({"changeset_id": cs.id, "conflicts": []});
    if landed_ids(store, repo_id).contains(&cs.id) || is_superseded(&cs) { return Ok(empty); }
    let ordered_ids: std::collections::HashSet<String> = integration_order(store, repo_id)?.into_iter()
        .filter_map(|entry| entry["changeset_id"].as_str().map(str::to_string)).collect();
    let active: Vec<crate::store::ChangesetRow> = store.list_changesets(repo_id)?.into_iter().filter(|c| ordered_ids.contains(&c.id)).collect();
    let Some(my_pos) = active.iter().position(|c| c.id == cs.id) else { return Ok(empty) };
    let mh = main_head(repo, cfg)?;
    let my_snap = live_snapshot(&cs);
    let my_base = git::merge_base(repo, &mh, &my_snap).unwrap_or_else(|_| cs.base_commit.clone());
    let mut conflicts = Vec::new();
    let sim_main = git::merge_sim(repo, &my_base, &mh, &my_snap);
    if !sim_main.clean {
        for d in git::merge_tree_conflict_hunks(repo, &my_base, &mh, &my_snap, &sim_main.conflict_files) {
            conflicts.push(serde_json::json!({"against": cfg.repo.main_branch, "file": d.file, "hunks": d.hunks}));
        }
    }
    for other in &active[..my_pos] {
        let other_snap = live_snapshot(other);
        if other_snap == my_snap { continue; }
        let base = git::merge_base(repo, &other_snap, &my_snap).unwrap_or_else(|_| my_base.clone());
        let sim = git::merge_sim(repo, &base, &other_snap, &my_snap);
        if !sim.clean {
            for d in git::merge_tree_conflict_hunks(repo, &base, &other_snap, &my_snap, &sim.conflict_files) {
                conflicts.push(serde_json::json!({"against": other.id, "file": d.file, "hunks": d.hunks}));
            }
        }
    }
    Ok(serde_json::json!({"changeset_id": cs.id, "conflicts": conflicts}))
}

/// Text form of `changeset_conflicts`: one file per line group with its
/// hunk headers indented under it; an explicit "no conflicts" when empty.
pub fn render_conflicts_text(v: &serde_json::Value) -> String {
    let id = v["changeset_id"].as_str().unwrap_or("");
    let conflicts = v["conflicts"].as_array().cloned().unwrap_or_default();
    if conflicts.is_empty() { return format!("{id}: no conflicts\n"); }
    let mut s = format!("{id} conflicts:\n");
    for c in &conflicts {
        s.push_str(&format!("  {} (vs {})\n", c["file"].as_str().unwrap_or(""), c["against"].as_str().unwrap_or("")));
        let hunks = c["hunks"].as_array().cloned().unwrap_or_default();
        if hunks.is_empty() {
            s.push_str("    (no hunk headers — non-content conflict)\n");
        }
        for h in &hunks { s.push_str(&format!("    {}\n", h.as_str().unwrap_or(""))); }
    }
    s
}
