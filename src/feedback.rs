//! Phase 2.3 self-reporting: agents (and Tower itself) file GitHub issues
//! about Tower's own bugs and inefficiencies. Reports persist in a local
//! outbox, are coalesced by signature, scrubbed of secrets/paths, and
//! filed best-effort via the `gh` CLI. Reporting never fails the command
//! that triggered it: every auto-capture entry point swallows its own
//! errors. Never includes file contents, diffs, code, secrets, tokens,
//! or environment values.

use anyhow::{bail, Result};
use std::path::Path;
use std::time::Duration;

use crate::config::TowerConfig;
use crate::store::{now_secs, ReportRow, Store};

pub const SLOW_THRESHOLD: Duration = Duration::from_secs(5);
pub const DAILY_CAP: i64 = 5;
pub const LABEL: &str = "tower-auto-report";

pub fn slow_threshold() -> Duration {
    match std::env::var("TOWER_SLOW_REPORT_MS") {
        Ok(v) => v.parse::<u64>().map(Duration::from_millis).unwrap_or(SLOW_THRESHOLD),
        Err(_) => SLOW_THRESHOLD,
    }
}

pub fn gh_bin() -> String {
    std::env::var("TOWER_GH").ok().filter(|v| !v.is_empty()).unwrap_or_else(|| "gh".into())
}

// ---------- signature ----------

/// Lowercase, collapse whitespace, strip varying IDs/numbers (digit runs
/// and long hex tokens become `#`).
pub fn normalize(s: &str) -> String {
    let mut out_tokens: Vec<String> = Vec::new();
    for tok in s.to_lowercase().split_whitespace() {
        let is_long_hex = tok.len() >= 8 && tok.chars().all(|c| c.is_ascii_hexdigit()) && tok.chars().any(|c| c.is_ascii_digit());
        if is_long_hex { out_tokens.push("#".into()); continue; }
        let mut t = String::new();
        let mut in_digits = false;
        for c in tok.chars() {
            if c.is_ascii_digit() {
                if !in_digits { t.push('#'); in_digits = true; }
            } else { in_digits = false; t.push(c); }
        }
        let t = t.trim_matches(|c: char| !(c.is_alphanumeric() || c == '#' || c == '-' || c == '_' || c == '/' || c == '.'));
        if !t.is_empty() { out_tokens.push(t.to_string()); }
    }
    out_tokens.join(" ")
}

/// Stable FNV-1a hash of (kind + normalized title + normalized key).
pub fn signature(kind: &str, title: &str, key: &str) -> String {
    let material = format!("{}|{}|{}", kind, normalize(title), normalize(key));
    let mut h: u64 = 0xcbf29ce484222325;
    for b in material.as_bytes() { h ^= *b as u64; h = h.wrapping_mul(0x100000001b3); }
    format!("{h:016x}")
}

// ---------- scrubbing ----------

fn is_sensitive_name(name: &str) -> bool {
    let n = name.to_lowercase();
    ["key", "token", "secret", "password", "auth"].iter().any(|w| n.contains(w))
}

/// Scrub with explicit home/repo prefixes (testable). Strips the home
/// prefix, prefers repo-relative paths, and redacts the value of any
/// flag/assignment whose name contains key/token/secret/password/auth.
pub fn scrub_with(s: &str, home: Option<&str>, repo: Option<&str>) -> String {
    let mut text = s.to_string();
    if let Some(r) = repo {
        if !r.is_empty() { text = text.replace(&format!("{r}/"), ""); text = text.replace(r, "."); }
    }
    if let Some(h) = home {
        if !h.is_empty() { text = text.replace(&format!("{h}/"), "~/"); text = text.replace(h, "~"); }
    }
    text.lines().map(scrub_flag_values).collect::<Vec<_>>().join("\n")
}

pub fn scrub(s: &str, repo: Option<&Path>) -> String {
    let home = std::env::var("HOME").ok();
    scrub_with(s, home.as_deref(), repo.map(|p| p.to_string_lossy().into_owned()).as_deref())
}

fn scrub_flag_values(s: &str) -> String {
    let tokens: Vec<&str> = s.split_whitespace().collect();
    let mut out: Vec<String> = Vec::with_capacity(tokens.len());
    let mut redact_next = false;
    for tok in tokens {
        if redact_next { out.push("[redacted]".into()); redact_next = false; continue; }
        // --flag=value, -flag=value, NAME=value, name:value
        if let Some((name, _val)) = tok.split_once('=') {
            let bare = name.trim_start_matches('-');
            if is_sensitive_name(bare) { out.push(format!("{name}=[redacted]")); continue; }
        }
        if let Some((name, _val)) = tok.split_once(':') {
            if !name.contains('/') && is_sensitive_name(name.trim_start_matches('-')) && !_val.is_empty() {
                out.push(format!("{name}:[redacted]")); continue;
            }
        }
        // --flag value  (also bare name: value pairs like `token: x`)
        let bare = tok.trim_start_matches('-').trim_end_matches(':');
        if (tok.starts_with('-') || tok.ends_with(':')) && is_sensitive_name(bare) {
            out.push(tok.to_string()); redact_next = true; continue;
        }
        out.push(tok.to_string());
    }
    if redact_next { /* flag was last token; nothing leaked */ }
    out.join(" ")
}

// ---------- classification / caps ----------

/// True only for failures that are Tower's fault. Expected user errors
/// (unknown IDs, unregistered repo, missing args, refused claims,
/// non-landable changesets) are not auto-reported.
pub fn is_internal_error(msg: &str) -> bool {
    let m = msg.to_lowercase();
    let user = ["unknown", "not registered", "not a git repo", "missing string arg",
        "missing arg", "claim refused", "lease held", "not landable", "refuses",
        "no nodes granted", "invalid kind", "kind must be"];
    if user.iter().any(|w| m.contains(w)) { return false; }
    let internal = ["panic", "invariant", "internal", "contradict", "mismatch tower cannot explain",
        "claimed-vs-actual mismatch", "database", "sqlite", "rusqlite", "corrupt",
        "constraint failed", "no such table", "no such column",
        "merge-tree", "merge_tree", "worktree add failed", "failed to run git"];
    internal.iter().any(|w| m.contains(w))
}

pub fn can_file(filed_today: i64, cap: i64) -> bool { filed_today < cap }

pub fn threshold_crossed(prev: i64, new: i64) -> Option<i64> {
    [10i64, 100, 1000].into_iter().find(|t| prev < *t && new >= *t)
}

pub fn utc_day_start(now: i64) -> i64 { now - now.rem_euclid(86_400) }

/// Minimal UTC ISO-8601 from epoch seconds (no chrono dependency).
pub fn iso8601(now: i64) -> String {
    let days = now.div_euclid(86_400);
    let secs = now.rem_euclid(86_400);
    let (hh, mm, ss) = (secs / 3600, (secs % 3600) / 60, secs % 60);
    // civil-from-days (Howard Hinnant's algorithm)
    let z = days + 719_468;
    let era = z.div_euclid(146_097);
    let doe = z.rem_euclid(146_097);
    let yoe = (doe - doe / 1460 + doe / 36_524 - doe / 146_096) / 365;
    let y = yoe + era * 400;
    let doy = doe - (365 * yoe + yoe / 4 - yoe / 100);
    let mp = (5 * doy + 2) / 153;
    let d = doy - (153 * mp + 2) / 5 + 1;
    let mo = if mp < 10 { mp + 3 } else { mp - 9 };
    let yr = if mo <= 2 { y + 1 } else { y };
    format!("{yr:04}-{mo:02}-{d:02}T{hh:02}:{mm:02}:{ss:02}Z")
}

// ---------- context + body ----------

fn auto_context(store: &Store, repo_id: i64, repo_root: &Path, command: Option<&str>, kind: &str) -> serde_json::Value {
    let recent = |ids: Vec<String>| ids.into_iter().rev().take(5).collect::<Vec<_>>();
    let task_ids = store.list_tasks(repo_id).map(|v| recent(v.iter().filter(|t| t.state != "failed").map(|t| t.id.clone()).collect())).unwrap_or_default();
    let asg_ids = store.list_assignments(repo_id).map(|v| recent(v.iter().filter(|a| a.state == "open").map(|a| a.id.clone()).collect())).unwrap_or_default();
    let cs_ids = store.list_changesets(repo_id).map(|v| recent(v.iter().map(|c| c.id.clone()).collect())).unwrap_or_default();
    let now = now_secs();
    let mut ctx = serde_json::json!({
        "tower_version": env!("CARGO_PKG_VERSION"),
        "os": std::env::consts::OS,
        "arch": std::env::consts::ARCH,
        "timestamp": iso8601(now),
        "timestamp_epoch": now,
        "kind": kind,
        "task_ids": task_ids,
        "assignment_ids": asg_ids,
        "changeset_ids": cs_ids,
    });
    if let Some(c) = command { ctx["command"] = serde_json::json!(scrub(c, Some(repo_root))); }
    ctx
}

/// Issue body: report body + auto context block + signature marker line.
pub fn issue_body(row: &ReportRow) -> String {
    let ctx: serde_json::Value = serde_json::from_str(&row.context_json).unwrap_or(serde_json::json!({}));
    format!(
        "{}\n\n---\nTower auto-report context:\n```json\n{}\n```\nOccurrences: {}\nFirst seen: {}\nLast seen: {}\n<!-- tower-signature: {} -->",
        row.body,
        serde_json::to_string_pretty(&ctx).unwrap_or_else(|_| "{}".into()),
        row.occurrence_count, iso8601(row.first_seen), iso8601(row.last_seen), row.signature
    )
}

pub fn report_value(row: &ReportRow) -> serde_json::Value {
    serde_json::json!({
        "report_id": row.id, "signature": row.signature, "kind": row.kind,
        "title": row.title, "body": row.body, "state": row.state,
        "occurrence_count": row.occurrence_count,
        "first_seen": row.first_seen, "last_seen": row.last_seen,
        "filed_url": row.filed_url, "filed_at": row.filed_at,
    })
}

// ---------- gh ----------

fn run_gh(gh: &str, args: &[&str]) -> Option<(bool, String)> {
    let out = std::process::Command::new(gh).args(args).output().ok()?;
    let mut s = String::from_utf8_lossy(&out.stdout).into_owned();
    s.push_str(&String::from_utf8_lossy(&out.stderr));
    Some((out.status.success(), s))
}

fn first_url(text: &str) -> Option<String> {
    for tok in text.split_whitespace() {
        let t = tok.trim_matches(|c: char| c == '"' || c == '\'' || c == ',' || c == ']' || c == '}');
        if let Some(i) = t.find("https://") {
            let u = &t[i..];
            if u.contains("github.com") || u.contains("/issues/") { return Some(u.to_string()); }
            return Some(u.to_string());
        }
    }
    None
}

/// Remote dedupe: look for this signature among open labelled issues.
/// Returns the existing issue URL when the signature marker is found.
fn remote_existing_url(gh: &str, feedback_repo: &str, sig: &str, list_output: &str) -> Option<String> {
    let _ = (gh, feedback_repo);
    if !list_output.contains(sig) { return None; }
    if let Ok(v) = serde_json::from_str::<serde_json::Value>(list_output) {
        if let Some(arr) = v.as_array() {
            for issue in arr {
                let hay = format!("{} {} {}", issue.get("body").and_then(|x| x.as_str()).unwrap_or(""), issue.get("title").and_then(|x| x.as_str()).unwrap_or(""), issue);
                if hay.contains(sig) {
                    if let Some(u) = issue.get("url").and_then(|x| x.as_str()) { return Some(u.to_string()); }
                }
            }
        }
    }
    first_url(list_output)
}

// ---------- create / flush ----------

pub fn create_report_with_gh(store: &Store, repo_id: i64, repo_root: &Path, cfg: &TowerConfig, kind: &str, title: &str, body: &str, command: Option<&str>, gh: &str) -> Result<serde_json::Value> {
    if kind != "bug" && kind != "inefficiency" { bail!("invalid kind: {kind} (kind must be bug or inefficiency)"); }
    if title.trim().is_empty() { bail!("missing arg: title"); }
    let clean_title = scrub(title, Some(repo_root));
    let clean_body = scrub(body, Some(repo_root));
    let key = command.map(|c| scrub(c, Some(repo_root))).unwrap_or_default();
    let sig = signature(kind, &clean_title, &key);
    let ctx = auto_context(store, repo_id, repo_root, command, kind);
    let (row, _created, prev) = store.upsert_report(repo_id, &sig, kind, &clean_title, &clean_body, &ctx.to_string())?;
    // Occurrence-threshold comment on an already-filed issue (best-effort).
    if row.state == "filed" {
        if let Some(t) = threshold_crossed(prev, row.occurrence_count) {
            if t > row.last_commented_threshold {
                if let Some(url) = &row.filed_url {
                    let text = format!("Tower auto-report: this signature has now occurred {t} times (last seen {}).", iso8601(row.last_seen));
                    let _ = run_gh(gh, &["issue", "comment", url, "--repo", &cfg.feedback.repo, "--body", &text]);
                    let _ = store.set_report_comment_threshold(repo_id, &sig, t);
                }
            }
        }
        return Ok(report_value(&store.report_by_signature(repo_id, &sig)?.unwrap_or(row)));
    }
    // Immediate flush attempt (best-effort, silent on failure).
    let _ = flush_reports_with_gh(store, repo_id, cfg, gh);
    Ok(report_value(&store.report_by_signature(repo_id, &sig)?.unwrap_or(row)))
}

pub fn create_report(store: &Store, repo_id: i64, repo_root: &Path, cfg: &TowerConfig, kind: &str, title: &str, body: &str, command: Option<&str>) -> Result<serde_json::Value> {
    create_report_with_gh(store, repo_id, repo_root, cfg, kind, title, body, command, &gh_bin())
}

/// Best-effort flush of the queued outbox. Never errors: missing/
/// unauthenticated/offline `gh` leaves reports queued. Returns a
/// summary value {filed, queued, attempted}.
pub fn flush_reports_with_gh(store: &Store, repo_id: i64, cfg: &TowerConfig, gh: &str) -> serde_json::Value {
    let queued_now = || store.queued_reports(repo_id).map(|v| v.len() as i64).unwrap_or(0);
    if !cfg.feedback.enabled {
        return serde_json::json!({"filed": 0, "queued": queued_now(), "attempted": 0, "enabled": false});
    }
    let queued = match store.queued_reports(repo_id) { Ok(v) => v, Err(_) => return serde_json::json!({"filed": 0, "queued": 0, "attempted": 0}) };
    if queued.is_empty() { return serde_json::json!({"filed": 0, "queued": 0, "attempted": 0, "enabled": true}); }
    // One remote list per flush for dedupe (best-effort).
    let list_output = run_gh(gh, &["issue", "list", "--repo", &cfg.feedback.repo, "--label", LABEL, "--state", "open", "--limit", "100"])
        .and_then(|(ok, s)| if ok { Some(s) } else { None }).unwrap_or_default();
    // Richer JSON listing when supported; fall back to the plain one.
    let list_output = match run_gh(gh, &["issue", "list", "--repo", &cfg.feedback.repo, "--label", LABEL, "--state", "open", "--limit", "100", "--json", "url,title,body"]) {
        Some((true, s)) if s.trim_start().starts_with('[') => s,
        _ => list_output,
    };
    let mut filed = 0i64;
    let mut attempted = 0i64;
    let day_start = utc_day_start(now_secs());
    for row in &queued {
        let today = store.filed_since_count(repo_id, day_start).unwrap_or(0);
        if !can_file(today, DAILY_CAP) { break; }
        if let Some(url) = remote_existing_url(gh, &cfg.feedback.repo, &row.signature, &list_output) {
            attempted += 1;
            if store.mark_report_filed(repo_id, &row.signature, &url).is_ok() { filed += 1; }
            continue;
        }
        attempted += 1;
        let ib = issue_body(row);
        match run_gh(gh, &["issue", "create", "--repo", &cfg.feedback.repo, "--title", &row.title, "--body", &ib, "--label", LABEL, "--label", &row.kind]) {
            Some((true, out)) => {
                let url = first_url(&out).unwrap_or_else(|| "https://github.com/unknown".into());
                if store.mark_report_filed(repo_id, &row.signature, &url).is_ok() { filed += 1; }
            }
            _ => { /* stay queued; silent */ }
        }
    }
    serde_json::json!({"filed": filed, "queued": queued_now(), "attempted": attempted, "enabled": true})
}

pub fn flush_reports(store: &Store, repo_id: i64, cfg: &TowerConfig) -> serde_json::Value {
    flush_reports_with_gh(store, repo_id, cfg, &gh_bin())
}

/// Silent opportunistic flush (board / changeset status start).
pub fn flush_best_effort(store: &Store, repo_id: i64, cfg: &TowerConfig) {
    let _ = flush_reports(store, repo_id, cfg);
}

// ---------- auto-capture (side effects that can never fail the caller) ----------

pub fn maybe_report_slow_with_gh(store: &Store, repo_id: i64, repo_root: &Path, cfg: &TowerConfig, operation: &str, elapsed: Duration, threshold: Duration, gh: &str) {
    if elapsed <= threshold || operation == "watch" || operation == "serve-mcp" || operation == "report" { return; }
    let title = format!("Slow operation: {operation} took {:.1}s", elapsed.as_secs_f64());
    let body = format!("Tower operation `{operation}` took {:.3}s, over the {}s slow-operation threshold.", elapsed.as_secs_f64(), threshold.as_secs());
    let _ = create_report_with_gh(store, repo_id, repo_root, cfg, "inefficiency", &title, &body, Some(operation), gh);
}

pub fn maybe_report_slow(store: &Store, repo_id: i64, repo_root: &Path, cfg: &TowerConfig, operation: &str, elapsed: Duration) {
    maybe_report_slow_with_gh(store, repo_id, repo_root, cfg, operation, elapsed, slow_threshold(), &gh_bin());
}

pub fn capture_internal_error_with_gh(store: &Store, repo_id: i64, repo_root: &Path, cfg: &TowerConfig, operation: &str, error: &str, command: &str, gh: &str) {
    if operation == "report" { return; }
    if !is_internal_error(error) { return; }
    let first = error.lines().next().unwrap_or(error);
    let title = format!("Internal error in {operation}: {}", first.chars().take(120).collect::<String>());
    let body = format!("Tower reported an internal error during `{operation}`.\n\nError: {}", scrub(error, Some(repo_root)));
    let _ = create_report_with_gh(store, repo_id, repo_root, cfg, "bug", &title, &body, Some(command), gh);
}

pub fn capture_internal_error(store: &Store, repo_id: i64, repo_root: &Path, cfg: &TowerConfig, operation: &str, error: &str, command: &str) {
    capture_internal_error_with_gh(store, repo_id, repo_root, cfg, operation, error, command, &gh_bin());
}

pub fn capture_panic(store: &Store, repo_id: i64, repo_root: &Path, cfg: &TowerConfig, operation: &str, detail: &str, command: &str) {
    let title = format!("Internal error in {operation}: panicked: {}", detail.chars().take(100).collect::<String>());
    let body = format!("Tower panicked during `{operation}` (caught at the dispatch boundary).\n\nPanic: {}", scrub(detail, Some(repo_root)));
    let _ = create_report(store, repo_id, repo_root, cfg, "bug", &title, &body, Some(command));
}
