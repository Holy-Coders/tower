use anyhow::{bail, Context, Result};
use std::io::{BufRead, Write};
use std::path::PathBuf;

use crate::board;
use crate::config::TowerConfig;
use crate::engine;
use crate::git;
use crate::store::{ClaimNode, Store};

pub fn resolve(store: &Store, repo_arg: &str) -> Result<(i64, PathBuf, TowerConfig)> {
    let p = PathBuf::from(repo_arg);
    let root = git::repo_root(&p).with_context(|| format!("not a git repo: {repo_arg}"))?;
    let canon = git::canonical_str(&root);
    let cfg = TowerConfig::load(&root);
    let repo = store.repo_by_path(&canon)?.with_context(|| format!("repo not registered: {canon} (run tower init)"))?;
    Ok((repo.id, root, cfg))
}

fn s(args: &serde_json::Value, key: &str) -> Result<String> {
    args.get(key).and_then(|v| v.as_str()).map(|v| v.to_string()).with_context(|| format!("missing string arg: {key}"))
}

/// MCP dispatch wrapped with Phase 2.3 auto-capture (timing, internal
/// errors, panics). Reporting is a side effect: it never changes the
/// tool call's outcome.
pub fn dispatch(store: &Store, name: &str, args: &serde_json::Value) -> Result<serde_json::Value> {
    let start = std::time::Instant::now();
    let caught = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| dispatch_inner(store, name, args)));
    let elapsed = start.elapsed();
    let result = match caught {
        Ok(r) => r,
        Err(payload) => {
            let detail = payload.downcast_ref::<&str>().map(|v| v.to_string())
                .or_else(|| payload.downcast_ref::<String>().cloned())
                .unwrap_or_else(|| "unknown panic".into());
            if let Some(repo) = args.get("repo").and_then(|v| v.as_str()) {
                if let Ok((rid, root, cfg)) = resolve(store, repo) {
                    crate::feedback::capture_panic(store, rid, &root, &cfg, name, &detail, &format!("mcp:{name} {}", crate::feedback::scrub(&args.to_string(), Some(&root))));
                }
            }
            std::panic::resume_unwind(payload);
        }
    };
    if name != "report_create" {
        if let Some(repo) = args.get("repo").and_then(|v| v.as_str()) {
            if let Ok((rid, root, cfg)) = resolve(store, repo) {
                let cmd = format!("mcp:{name} {}", crate::feedback::scrub(&args.to_string(), Some(&root)));
                if let Err(e) = &result {
                    crate::feedback::capture_internal_error(store, rid, &root, &cfg, name, &format!("{e:#}"), &cmd);
                }
                crate::feedback::maybe_report_slow(store, rid, &root, &cfg, name, elapsed);
            }
        }
    }
    result
}

fn dispatch_inner(store: &Store, name: &str, args: &serde_json::Value) -> Result<serde_json::Value> {
    match name {
        "board_get" => { let (rid, root, cfg) = resolve(store, &s(args, "repo")?)?; crate::feedback::flush_best_effort(store, rid, &cfg); board::build_board(store, rid, &root, &cfg) }
        "worktree_list" => {
            let (_, root, cfg) = resolve(store, &s(args, "repo")?)?;
            let _ = cfg;
            let list = git::worktree_list(&root)?;
            Ok(serde_json::to_value(list)?)
        }
        "events_since" => {
            let (rid, _, _) = resolve(store, &s(args, "repo")?)?;
            let cursor = args.get("cursor").and_then(|v| v.as_i64()).unwrap_or(0);
            let ev = store.events_since(rid, cursor)?;
            Ok(serde_json::json!({"events": ev.iter().map(|e| serde_json::json!({"seq": e.seq, "kind": e.kind, "payload": serde_json::from_str::<serde_json::Value>(&e.payload_json).unwrap_or_default(), "created_at": e.created_at})).collect::<Vec<_>>(), "next_cursor": ev.last().map(|e| e.seq).unwrap_or(cursor)}))
        }
        "task_create" => {
            let (rid, _, _) = resolve(store, &s(args, "repo")?)?;
            let t = store.create_task(rid, &s(args, "title")?, &args.get("brief").and_then(|v| v.as_str()).unwrap_or("").to_string())?;
            Ok(serde_json::json!({"task_id": t.id, "state": t.state}))
        }
        "task_plan_save" => {
            let (rid, _, _) = resolve(store, &s(args, "repo")?)?;
            let plan = match args.get("plan") { Some(v) if v.is_string() => v.as_str().unwrap().to_string(), Some(v) => v.to_string(), None => bail!("missing arg: plan") };
            store.save_plan(rid, &s(args, "task_id")?, &plan)?;
            Ok(serde_json::json!({"saved": true}))
        }
        "claim_acquire" => {
            let (rid, _, _) = resolve(store, &s(args, "repo")?)?;
            let nodes: Vec<ClaimNode> = serde_json::from_value(args.get("nodes").cloned().unwrap_or_default())?;
            let r = store.acquire_claims_partial(rid, &s(args, "assignment_id")?, &nodes)?;
            Ok(serde_json::json!({"granted": r.refused.is_empty(), "granted_nodes": r.granted_nodes, "refused": r.refused.clone(), "collisions": r.refused}))
        }
        "claim_release" => {
            let (rid, _, _) = resolve(store, &s(args, "repo")?)?;
            let nodes: Option<Vec<ClaimNode>> = match args.get("nodes") { Some(v) if !v.is_null() => Some(serde_json::from_value(v.clone())?), _ => None };
            store.release_claims_nodes(rid, &s(args, "assignment_id")?, nodes.as_deref())?;
            Ok(serde_json::json!({"released": true, "nodes": nodes}))
        }
        "worktree_create" => {
            let (rid, root, cfg) = resolve(store, &s(args, "repo")?)?;
            crate::ops::worktree_create(store, rid, &root, &cfg, &s(args, "task_id")?)
        }
        "changeset_register" => {
            let (rid, root, cfg) = resolve(store, &s(args, "repo")?)?;
            engine::register_changeset(store, rid, &root, &cfg, &s(args, "assignment_id")?, &s(args, "worktree")?)
        }
        "changeset_status" => {
            let (rid, root, cfg) = resolve(store, &s(args, "repo")?)?;
            crate::feedback::flush_best_effort(store, rid, &cfg);
            board::changeset_status_value(store, rid, &root, &cfg, &s(args, "changeset_id")?)
        }
        "report_create" => {
            let (rid, root, cfg) = resolve(store, &s(args, "repo")?)?;
            crate::feedback::create_report(store, rid, &root, &cfg, &s(args, "kind")?, &s(args, "title")?, &s(args, "body")?, args.get("command").and_then(|v| v.as_str()))
        }
        "changeset_refresh" => {
            let (rid, root, cfg) = resolve(store, &s(args, "repo")?)?;
            engine::refresh_changeset(store, rid, &root, &cfg, &s(args, "changeset_id")?)
        }
        "changeset_supersede" => {
            let (rid, root, cfg) = resolve(store, &s(args, "repo")?)?;
            engine::supersede_changeset(store, rid, &root, &cfg, &s(args, "changeset_id")?)
        }
        "changeset_conflicts" => {
            let (rid, root, cfg) = resolve(store, &s(args, "repo")?)?;
            engine::changeset_conflicts(store, rid, &root, &cfg, &s(args, "changeset_id")?)
        }
        "integration_order" => { let (rid, _, _) = resolve(store, &s(args, "repo")?)?; Ok(serde_json::json!({"order": engine::integration_order(store, rid)?})) }
        "gate_run" => {
            let (rid, root, cfg) = resolve(store, &s(args, "repo")?)?;
            engine::gate_run_single(store, rid, &root, &cfg, &s(args, "changeset_id")?, args.get("tier").and_then(|v| v.as_i64()).unwrap_or(1))
        }
        "escalate_create" => {
            let (rid, _, _) = resolve(store, &s(args, "repo")?)?;
            let e = store.create_escalation(rid, &s(args, "task_id")?, &s(args, "question")?, &args.get("options").cloned().unwrap_or(serde_json::json!([])), &args.get("recommendation").and_then(|v| v.as_str()).unwrap_or("").to_string())?;
            Ok(serde_json::json!({"escalation_id": e.id}))
        }
        "escalate_resolve" => { let (rid, _, _) = resolve(store, &s(args, "repo")?)?; store.resolve_escalation(rid, &s(args, "escalation_id")?, &s(args, "answer")?)?; Ok(serde_json::json!({"resolved": true})) }
        "decision_list" => { let (rid, _, _) = resolve(store, &s(args, "repo")?)?; Ok(serde_json::json!({"decisions": store.list_decisions(rid)?})) }
        "decision_log" => { let (rid, _, _) = resolve(store, &s(args, "repo")?)?; let d = store.log_decision(rid, &s(args, "text")?)?; Ok(serde_json::json!({"decision_id": d.id})) }
        _ => bail!("unknown tool: {name}"),
    }
}

pub fn tool_defs() -> serde_json::Value {
    // Full input schemas so an MCP client can call every tool from the
    // listing alone. `repo` (path to a registered repo) is common to all.
    let t = |name: &str, desc: &str, mut props: serde_json::Map<String, serde_json::Value>, required: &[&str]| {
        props.entry("repo".to_string()).or_insert(serde_json::json!({"type": "string", "description": "Path to the registered repo"}));
        let mut req: Vec<&str> = vec!["repo"];
        req.extend(required.iter().copied());
        serde_json::json!({"name": name, "description": desc, "inputSchema": {"type": "object", "properties": props, "required": req}})
    };
    let p = |pairs: &[(&str, serde_json::Value)]| pairs.iter().map(|(k, v)| (k.to_string(), v.clone())).collect::<serde_json::Map<String, serde_json::Value>>();
    let st = serde_json::json!({"type": "string"});
    serde_json::json!([
        t("board_get", "Full board state", p(&[]), &[]),
        t("task_create", "Create task -> T-ID", p(&[("title", st.clone()), ("brief", st.clone())]), &["title"]),
        t("task_plan_save", "Store plan decomposition", p(&[("task_id", st.clone()), ("plan", st.clone())]), &["task_id", "plan"]),
        t("claim_acquire", "Acquire claims (partial grants: uncontested granted, rest refused by name)", p(&[("assignment_id", st.clone()), ("nodes", serde_json::json!({"type": "array", "items": {"type": "object", "properties": {"file": {"type": "string"}, "item": {"type": "string"}}, "required": ["file"]}}))]), &["assignment_id", "nodes"]),
        t("claim_release", "Release claims (all, or only the given nodes)", p(&[("assignment_id", st.clone()), ("nodes", serde_json::json!({"type": "array", "items": {"type": "object", "properties": {"file": {"type": "string"}, "item": {"type": "string"}}, "required": ["file"]}}))]), &["assignment_id"]),
        t("worktree_create", "Create Tower-managed worktree + assignment", p(&[("task_id", st.clone())]), &["task_id"]),
        t("worktree_list", "List worktrees", p(&[]), &[]),
        t("changeset_register", "Register changeset -> C-ID", p(&[("assignment_id", st.clone()), ("worktree", st.clone())]), &["assignment_id", "worktree"]),
        t("changeset_status", "Green/yellow/red with reasons + next action", p(&[("changeset_id", st.clone())]), &["changeset_id"]),
        t("changeset_refresh", "Recompute live CVA/head, bump version", p(&[("changeset_id", st.clone())]), &["changeset_id"]),
        t("changeset_supersede", "Manually supersede a changeset", p(&[("changeset_id", st.clone())]), &["changeset_id"]),
        t("changeset_conflicts", "Conflicting files + git merge-tree hunk headers vs main/earlier changesets (read-only)", p(&[("changeset_id", st.clone())]), &["changeset_id"]),
        t("integration_order", "Planned landing order", p(&[]), &[]),
        t("gate_run", "Run gates on rehearsal tree", p(&[("changeset_id", st.clone()), ("tier", serde_json::json!({"type": "integer", "enum": [1, 2]}))]), &["changeset_id"]),
        t("escalate_create", "Create escalation -> E-ID", p(&[("task_id", st.clone()), ("question", st.clone()), ("options", serde_json::json!({"type": "array", "items": {"type": "string"}})), ("recommendation", st.clone())]), &["task_id", "question"]),
        t("escalate_resolve", "Record human answer", p(&[("escalation_id", st.clone()), ("answer", st.clone())]), &["escalation_id", "answer"]),
        t("decision_list", "List decisions", p(&[]), &[]),
        t("decision_log", "Log decision -> D-ID", p(&[("text", st.clone())]), &["text"]),
        t("events_since", "Event log stream", p(&[("cursor", serde_json::json!({"type": "integer"}))]), &[]),
        t("report_create", "Report Tower itself: a command failed unexpectedly, output contradicts Tower's own state (e.g. board vs changeset status), CLI and MCP disagree, an operation was slow enough to disrupt your work, or you had to work around Tower. Do NOT report bugs in the code you are orchestrating — only bugs or inefficiencies in Tower.", p(&[("kind", serde_json::json!({"type": "string", "enum": ["bug", "inefficiency"]})), ("title", st.clone()), ("body", st.clone()), ("command", st.clone())]), &["kind", "title", "body"]),
    ])
}

pub fn serve(store: &Store) -> Result<()> {
    let stdin = std::io::stdin();
    let mut stdout = std::io::stdout();
    for line in stdin.lock().lines() {
        let line = line?;
        let line = line.trim();
        if line.is_empty() { continue; }
        let req: serde_json::Value = match serde_json::from_str(line) { Ok(v) => v, Err(_) => continue };
        let id = req.get("id").cloned().unwrap_or(serde_json::Value::Null);
        let method = req.get("method").and_then(|v| v.as_str()).unwrap_or("");
        if method.starts_with("notifications/") || id.is_null() && method.is_empty() { continue; }
        let resp = match method {
            "initialize" => serde_json::json!({"jsonrpc": "2.0", "id": id, "result": {"protocolVersion": "2024-11-05", "capabilities": {"tools": {}}, "serverInfo": {"name": "tower", "version": env!("CARGO_PKG_VERSION")}}}),
            "ping" => serde_json::json!({"jsonrpc": "2.0", "id": id, "result": {}}),
            "tools/list" => serde_json::json!({"jsonrpc": "2.0", "id": id, "result": {"tools": tool_defs()}}),
            "tools/call" => {
                let params = req.get("params").cloned().unwrap_or_default();
                let tname = params.get("name").and_then(|v| v.as_str()).unwrap_or("");
                let targs = params.get("arguments").cloned().unwrap_or(serde_json::json!({}));
                match dispatch(store, tname, &targs) {
                    Ok(v) => serde_json::json!({"jsonrpc": "2.0", "id": id, "result": {"content": [{"type": "text", "text": v.to_string()}], "structuredContent": v}}),
                    Err(e) => serde_json::json!({"jsonrpc": "2.0", "id": id, "result": {"content": [{"type": "text", "text": format!("error: {e:#}")}], "isError": true}}),
                }
            }
            _ => serde_json::json!({"jsonrpc": "2.0", "id": id, "error": {"code": -32601, "message": format!("method not found: {method}")}}),
        };
        writeln!(stdout, "{}", resp)?;
        stdout.flush()?;
    }
    Ok(())
}
