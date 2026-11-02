use anyhow::{Context, Result};
use std::path::Path;

use crate::config::TowerConfig;
use crate::engine;
use crate::store::Store;

/// Pure next-action hint derived from a changeset's computed status and
/// reasons (Phase 2.1). No store or git access: same input, same hint.
pub fn next_action(status: &str, reasons: &[String], landed: bool, superseded: bool) -> String {
    if superseded || status == "superseded" { return "superseded".to_string(); }
    if landed { return "landed".to_string(); }
    let conflict = reasons.iter().find(|r| r.starts_with("conflicts with "));
    match status {
        "green" => "land".to_string(),
        "red" => {
            if let Some(r) = conflict {
                // "conflicts with X in a,b" -> "resolve conflict with X in a,b"
                format!("resolve conflict with {}", r.trim_start_matches("conflicts with "))
            } else if reasons.iter().any(|r| r.contains("gate failed")) {
                "fix failing gate".to_string()
            } else if let Some(r) = reasons.first() {
                format!("resolve: {r}")
            } else { "resolve conflict".to_string() }
        }
        "yellow" => {
            if reasons.iter().any(|r| r.contains("shared reconciliation pending")) { "merge verified shared result and validate consumer".to_string() }
            else if reasons.iter().any(|r| r.contains("gate proof stale")) { "rerun gates (proof stale)".to_string() }
            else if reasons.iter().any(|r| r.contains("outside claim")) { "refresh claims or changeset".to_string() }
            else if reasons.iter().any(|r| r.contains("has not passed yet") || r.contains("have not run")) { "run gates".to_string() }
            else { "refresh claims or changeset".to_string() }
        }
        _ => "review reasons".to_string(),
    }
}

fn reason_strings(reasons_json: &str) -> Vec<String> {
    serde_json::from_str::<serde_json::Value>(reasons_json).ok().and_then(|v| v.as_array().map(|a| a.iter().filter_map(|x| x.as_str().map(|s| s.to_string())).collect())).unwrap_or_default()
}

/// The `changeset status` payload, shared by the CLI and the MCP
/// `changeset_status` tool so the two surfaces cannot drift. Includes
/// `claimed_vs_actual` in the same shape as `board --json` (Phase 2.2
/// Fix C): the command that prints "refresh claims or changeset" names
/// what to claim.
pub fn changeset_status_value(store: &Store, repo_id: i64, repo: &Path, cfg: &TowerConfig, changeset_id: &str) -> Result<serde_json::Value> {
    engine::refresh_changeset_statuses(store, repo_id, repo, cfg)?;
    let c = store.get_changeset(repo_id, changeset_id)?.with_context(|| format!("unknown changeset {changeset_id}"))?;
    let reasons: Vec<String> = serde_json::from_str::<Vec<String>>(&c.reasons_json).unwrap_or_default();
    let landed = engine::landed_ids(store, repo_id).contains(&c.id);
    Ok(serde_json::json!({"changeset_id": c.id, "status": c.status, "version": c.version, "superseded": c.superseded, "superseded_by": c.superseded_by, "landed": landed, "next": next_action(&c.status, &reasons, landed, c.superseded), "reasons": serde_json::from_str::<serde_json::Value>(&c.reasons_json).unwrap_or_default(), "claimed_vs_actual": serde_json::from_str::<serde_json::Value>(&c.claimed_vs_actual_json).unwrap_or_default()}))
}

pub fn build_board(store: &Store, repo_id: i64, repo: &Path, cfg: &TowerConfig) -> Result<serde_json::Value> {
    let radar = engine::radar_scan(store, repo, cfg)?;
    // map collision endpoints to labels (changeset/assignment ids when known)
    let collisions: Vec<serde_json::Value> = radar.collisions.iter().map(|c| {
        serde_json::json!({"a": c.a, "a_label": engine::label_for(store, repo_id, &c.a), "b": c.b, "b_label": if c.b.starts_with("branch:") { c.b.clone() } else { engine::label_for(store, repo_id, &c.b) }, "files": c.files})
    }).collect();
    let worktrees: Vec<serde_json::Value> = radar.worktrees.iter().map(|w| {
        serde_json::json!({"path": w.path, "label": engine::label_for(store, repo_id, &w.path), "branch": w.branch, "head": w.head, "snapshot": w.snapshot, "base": w.base, "status": w.status, "reasons": w.reasons, "files": w.files, "changed_items": w.changed_item_names, "items": w.items})
    }).collect();
    let tasks: Vec<serde_json::Value> = store.list_tasks(repo_id)?.iter().map(|t| serde_json::json!({"id": t.id, "title": t.title, "brief": t.brief, "plan": t.plan, "state": t.state})).collect();
    let assignments: Vec<serde_json::Value> = store.list_assignments(repo_id)?.iter().map(|a| serde_json::json!({"id": a.id, "task_id": a.task_id, "worktree": a.worktree_path, "branch": a.branch, "state": a.state})).collect();
    let landed = engine::landed_ids(store, repo_id);
    let all_changesets = store.list_changesets(repo_id)?;
    let changesets: Vec<serde_json::Value> = all_changesets.iter().filter(|c| !engine::is_superseded(c)).map(|c| {
        let reasons = reason_strings(&c.reasons_json);
        let is_landed = landed.contains(&c.id);
        serde_json::json!({"id": c.id, "task_id": c.task_id, "assignment_id": c.assignment_id, "worktree": c.worktree_path, "branch": c.branch, "status": c.status, "version": c.version, "landed": is_landed, "superseded": false, "superseded_by": c.superseded_by, "next": next_action(&c.status, &reasons, is_landed, false), "reasons": serde_json::from_str::<serde_json::Value>(&c.reasons_json).unwrap_or_default(), "files": serde_json::from_str::<serde_json::Value>(&c.files_json).unwrap_or_default(), "claimed_vs_actual": serde_json::from_str::<serde_json::Value>(&c.claimed_vs_actual_json).unwrap_or_default()})
    }).collect();
    let superseded_changesets: Vec<serde_json::Value> = all_changesets.iter().filter(|c| engine::is_superseded(c)).map(|c| {
        serde_json::json!({"id": c.id, "task_id": c.task_id, "assignment_id": c.assignment_id, "status": c.status, "version": c.version, "superseded": true, "superseded_by": c.superseded_by, "next": "superseded"})
    }).collect();
    let claims: Vec<serde_json::Value> = store.list_claims(repo_id)?.iter().map(|c| serde_json::json!({"assignment_id": c.assignment_id, "file": c.file_path, "item": c.item_name})).collect();
    let escalations: Vec<serde_json::Value> = store.list_escalations(repo_id)?.iter().map(|e| serde_json::json!({"id": e.id, "task_id": e.task_id, "question": e.question, "options": serde_json::from_str::<serde_json::Value>(&e.options_json).unwrap_or_default(), "recommendation": e.recommendation, "answer": e.answer, "resolved": e.resolved})).collect();
    let decisions: Vec<serde_json::Value> = store.list_decisions(repo_id)?.iter().map(|d| serde_json::json!({"id": d.id, "text": d.text, "revoked": d.revoked})).collect();
    let lease = store.lease_holder(repo_id)?.map(|(h, t)| serde_json::json!({"holder": h, "acquired_at": t}));
    let order = engine::integration_order(store, repo_id)?;
    Ok(serde_json::json!({
        "repo": crate::git::canonical_str(repo),
        "main_branch": cfg.repo.main_branch,
        "tiers": cfg.tier_label(),
        "gates": {"check": cfg.gates.check, "test": cfg.gates.test},
        "lease": lease,
        "tasks": tasks, "assignments": assignments, "claims": claims, "changesets": changesets, "superseded_changesets": superseded_changesets,
        "worktrees": worktrees, "collisions": collisions, "integration_order": order,
        "scheduler": crate::scheduler::status(store,repo_id,cfg)?,
        "reconciliations": crate::reconcile::list(store,repo_id)?,
        "resource_queue": crate::resources::list(store,Some(repo_id))?,
        "escalations": escalations, "decisions": decisions,
        "generated_at": crate::store::now_secs(),
    }))
}

pub fn render_text(b: &serde_json::Value) -> String {
    let mut s = String::new();
    s.push_str(&format!("TOWER BOARD — {} (main: {})\n", b["repo"].as_str().unwrap_or(""), b["main_branch"].as_str().unwrap_or("")));
    s.push_str(&format!("Tiers: {}\n", b["tiers"].as_str().unwrap_or("")));
    if let Some(h) = b["lease"].get("holder").and_then(|v| v.as_str()) { s.push_str(&format!("Orchestrator lease: {h}\n")); } else { s.push_str("Orchestrator lease: (none)\n"); }
    s.push_str("\nTASKS\n");
    for t in b["tasks"].as_array().cloned().unwrap_or_default() { s.push_str(&format!("  {} [{}] {}\n", t["id"].as_str().unwrap_or(""), t["state"].as_str().unwrap_or(""), t["title"].as_str().unwrap_or(""))); }
    s.push_str("\nCOORDINATION\n");
    for r in b["scheduler"]["runs"].as_array().cloned().unwrap_or_default() {
        s.push_str(&format!("  run {} {} [{}] {}\n",r["id"],r["task_id"].as_str().unwrap_or(""),r["state"].as_str().unwrap_or(""),r["detail"].as_str().unwrap_or("")));
    }
    for r in b["reconciliations"].as_array().cloned().unwrap_or_default() {
        s.push_str(&format!("  reconciliation {} [{}] (inspect: tower reconcile show)\n",r["id"].as_str().unwrap_or(""),r["state"].as_str().unwrap_or("")));
    }
    for q in b["resource_queue"].as_array().cloned().unwrap_or_default().into_iter().filter(|q|matches!(q["state"].as_str(),Some("queued"|"running"))) {
        s.push_str(&format!("  resource job {} [{}] {}\n",q["id"],q["state"].as_str().unwrap_or(""),q["request_key"].as_str().unwrap_or("")));
    }
    s.push_str("\nWORKTREES\n");
    for w in b["worktrees"].as_array().cloned().unwrap_or_default() {
        s.push_str(&format!("  [{}] {} ({}) branch={} files={} items={}\n", w["status"].as_str().unwrap_or("").to_uppercase(), w["label"].as_str().unwrap_or(""), w["path"].as_str().unwrap_or(""), w["branch"].as_str().unwrap_or("(detached)"), w["files"].as_array().map(|a| a.len()).unwrap_or(0), w["changed_items"].as_array().map(|a| a.len()).unwrap_or(0)));
        for f in w["files"].as_array().cloned().unwrap_or_default() { s.push_str(&format!("      file: {}\n", f.as_str().unwrap_or(""))); }
        for r in w["reasons"].as_array().cloned().unwrap_or_default() { s.push_str(&format!("      - {}\n", r.as_str().unwrap_or(""))); }
    }
    s.push_str("\nCOLLISIONS\n");
    let cols = b["collisions"].as_array().cloned().unwrap_or_default();
    if cols.is_empty() { s.push_str("  (none)\n"); }
    for c in cols { s.push_str(&format!("  RED {} <-> {} in {}\n", c["a_label"].as_str().unwrap_or(""), c["b_label"].as_str().unwrap_or(""), c["files"].as_array().map(|a| a.iter().filter_map(|v| v.as_str().map(|x| x.to_string())).collect::<Vec<_>>().join(",")).unwrap_or_default())); }
    s.push_str("\nCHANGESETS\n");
    for c in b["changesets"].as_array().cloned().unwrap_or_default() {
        s.push_str(&format!("  {} [{}] task={} assignment={} files={} next: {}\n", c["id"].as_str().unwrap_or(""), c["status"].as_str().unwrap_or("").to_uppercase(), c["task_id"].as_str().unwrap_or(""), c["assignment_id"].as_str().unwrap_or(""), c["files"].to_string(), c["next"].as_str().unwrap_or("")));
        for r in c["reasons"].as_array().cloned().unwrap_or_default() {
            // Name the outside items/files on the reason line itself, even
            // for rows stored with the pre-2.2 plain reason text.
            let rs = r.as_str().unwrap_or("");
            let line = if rs == "touched files outside claim" { engine::outside_reason(&c["claimed_vs_actual"]) } else { rs.to_string() };
            s.push_str(&format!("      - {line}\n"));
        }
    }
    let superseded = b["superseded_changesets"].as_array().cloned().unwrap_or_default();
    if !superseded.is_empty() {
        s.push_str("\nSUPERSEDED CHANGESETS\n");
        for c in superseded {
            match c["superseded_by"].as_str() {
                Some(by) => s.push_str(&format!("  {} superseded by {}\n", c["id"].as_str().unwrap_or(""), by)),
                None => s.push_str(&format!("  {} superseded\n", c["id"].as_str().unwrap_or(""))),
            }
        }
    }
    s.push_str("\nCLAIMS\n");
    for c in b["claims"].as_array().cloned().unwrap_or_default() { s.push_str(&format!("  {} -> {}{}\n", c["assignment_id"].as_str().unwrap_or(""), c["file"].as_str().unwrap_or(""), c["item"].as_str().map(|i| format!("#{i}")).unwrap_or_default())); }
    s.push_str("\nINTEGRATION ORDER\n");
    for o in b["integration_order"].as_array().cloned().unwrap_or_default() { s.push_str(&format!("  {} ({})\n", o["changeset_id"].as_str().unwrap_or(""), o["reason"].as_str().unwrap_or(""))); }
    s.push_str("\nOPEN ESCALATIONS\n");
    for e in b["escalations"].as_array().cloned().unwrap_or_default() { if e["resolved"].as_bool() != Some(true) { s.push_str(&format!("  {} (for {}): {}\n", e["id"].as_str().unwrap_or(""), e["task_id"].as_str().unwrap_or(""), e["question"].as_str().unwrap_or(""))); } }
    s.push_str("\nDECISIONS\n");
    for d in b["decisions"].as_array().cloned().unwrap_or_default() { s.push_str(&format!("  {} {} {}\n", d["id"].as_str().unwrap_or(""), if d["revoked"].as_bool() == Some(true) { "[revoked]" } else { "" }, d["text"].as_str().unwrap_or(""))); }
    s
}

fn esc(s: &str) -> String { s.replace('&', "&amp;").replace('<', "&lt;").replace('>', "&gt;") }

pub fn render_html(b: &serde_json::Value) -> String {
    let mut h = String::from("<!DOCTYPE html><html><head><meta charset=\"utf-8\"><title>Tower Board</title><style>body{font-family:system-ui,sans-serif;margin:2em}table{border-collapse:collapse}td,th{border:1px solid #ccc;padding:4px 8px}.green{background:#d6f5d6}.yellow{background:#fff3b0}.red{background:#f8c8c8}</style></head><body>");
    h.push_str(&format!("<h1>Tower Board</h1><p>Repo: {}<br>Main: {}<br>Tiers: {}</p>", esc(b["repo"].as_str().unwrap_or("")), esc(b["main_branch"].as_str().unwrap_or("")), esc(b["tiers"].as_str().unwrap_or(""))));
    h.push_str("<h2>Worktrees</h2><table><tr><th>Status</th><th>Label</th><th>Path</th><th>Files</th><th>Reasons</th></tr>");
    for w in b["worktrees"].as_array().cloned().unwrap_or_default() {
        let st = w["status"].as_str().unwrap_or("");
        h.push_str(&format!("<tr class=\"{st}\"><td>{}</td><td>{}</td><td>{}</td><td>{}</td><td>{}</td></tr>", esc(st), esc(w["label"].as_str().unwrap_or("")), esc(w["path"].as_str().unwrap_or("")), esc(&w["files"].to_string()), esc(&w["reasons"].to_string())));
    }
    h.push_str("</table><h2>Collisions</h2><ul>");
    for c in b["collisions"].as_array().cloned().unwrap_or_default() { h.push_str(&format!("<li class=\"red\">{} &lt;-&gt; {} in {}</li>", esc(c["a_label"].as_str().unwrap_or("")), esc(c["b_label"].as_str().unwrap_or("")), esc(&c["files"].to_string()))); }
    h.push_str("</ul><h2>Changesets</h2><table><tr><th>ID</th><th>Status</th><th>Task</th><th>Files</th><th>Next</th></tr>");
    for c in b["changesets"].as_array().cloned().unwrap_or_default() {
        let st = c["status"].as_str().unwrap_or("");
        h.push_str(&format!("<tr class=\"{st}\"><td>{}</td><td>{}</td><td>{}</td><td>{}</td><td>{}</td></tr>", esc(c["id"].as_str().unwrap_or("")), esc(st), esc(c["task_id"].as_str().unwrap_or("")), esc(&c["files"].to_string()), esc(c["next"].as_str().unwrap_or(""))));
    }
    h.push_str("</table><h2>Superseded changesets</h2><ul>");
    for c in b["superseded_changesets"].as_array().cloned().unwrap_or_default() {
        h.push_str(&format!("<li>{} superseded{}</li>", esc(c["id"].as_str().unwrap_or("")), c["superseded_by"].as_str().map(|by| format!(" by {}", esc(by))).unwrap_or_default()));
    }
    h.push_str("</ul><h2>Tasks</h2><ul>");
    for t in b["tasks"].as_array().cloned().unwrap_or_default() { h.push_str(&format!("<li>{} [{}] {}</li>", esc(t["id"].as_str().unwrap_or("")), esc(t["state"].as_str().unwrap_or("")), esc(t["title"].as_str().unwrap_or("")))); }
    h.push_str("</ul></body></html>");
    h
}
