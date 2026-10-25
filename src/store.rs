use anyhow::{Context, Result};
use rusqlite::{params, Connection};
use serde::{Deserialize, Serialize};
use std::path::PathBuf;
use std::time::{SystemTime, UNIX_EPOCH};

pub fn now_secs() -> i64 {
    SystemTime::now().duration_since(UNIX_EPOCH).map(|d| d.as_secs() as i64).unwrap_or(0)
}

pub struct Store { pub conn: Connection, pub home: PathBuf }

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct RepoRow { pub id: i64, pub path: String, pub main_branch: String, pub created_at: i64 }
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct TaskRow { pub id: String, pub repo_id: i64, pub title: String, pub brief: String, pub plan: Option<String>, pub state: String, pub created_at: i64 }
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct AssignmentRow { pub id: String, pub repo_id: i64, pub task_id: String, pub worktree_path: String, pub branch: String, pub state: String, pub created_at: i64 }
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ClaimRow { pub id: i64, pub repo_id: i64, pub assignment_id: String, pub file_path: String, pub item_name: Option<String>, pub created_at: i64 }
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ClaimNode { pub file: String, #[serde(default)] pub item: Option<String> }
impl PartialEq for ClaimNode { fn eq(&self, other: &Self) -> bool { self.file == other.file && self.item == other.item } }
impl Eq for ClaimNode {}
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ChangesetRow { pub id: String, pub repo_id: i64, pub assignment_id: String, pub task_id: String, pub worktree_path: String, pub branch: String, pub base_commit: String, pub head_commit: String, pub files_json: String, pub items_json: String, pub claimed_vs_actual_json: String, pub status: String, pub reasons_json: String, pub created_at: i64, #[serde(default = "default_version")] pub version: i64, #[serde(default)] pub superseded: bool, #[serde(default)] pub superseded_by: Option<String> }
fn default_version() -> i64 { 1 }
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct EscalationRow { pub id: String, pub repo_id: i64, pub task_id: String, pub question: String, pub options_json: String, pub recommendation: String, pub answer: Option<String>, pub resolved: bool, pub created_at: i64 }
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct DecisionRow { pub id: String, pub repo_id: i64, pub text: String, pub revoked: bool, pub created_at: i64 }
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct EventRow { pub seq: i64, pub repo_id: i64, pub kind: String, pub payload_json: String, pub created_at: i64 }
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct GateRunRow { pub id: i64, pub repo_id: i64, pub changeset_id: String, pub tier: i64, pub command: String, pub passed: bool, pub output: String, pub created_at: i64, #[serde(default)] pub tree_hash: Option<String>, #[serde(default)] pub main_head: Option<String>, #[serde(default)] pub participants_json: Option<String> }

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct PartialClaimResult { pub granted_nodes: Vec<ClaimNode>, pub refused: Vec<serde_json::Value> }

impl Store {
    pub fn open(home: PathBuf) -> Result<Self> {
        std::fs::create_dir_all(&home).with_context(|| format!("create {}", home.display()))?;
        let db = home.join("tower.db");
        let conn = Connection::open(&db)?;
        conn.execute_batch("PRAGMA journal_mode=WAL; PRAGMA foreign_keys=ON;")?;
        let s = Self { conn, home };
        s.migrate()?;
        Ok(s)
    }
    #[cfg(test)]
    pub fn open_memory() -> Result<Self> {
        let conn = Connection::open_in_memory()?;
        let s = Self { conn, home: PathBuf::from("/tmp") };
        s.migrate()?;
        Ok(s)
    }
    fn migrate(&self) -> Result<()> {
        self.conn.execute_batch(
            "CREATE TABLE IF NOT EXISTS repos(id INTEGER PRIMARY KEY, path TEXT UNIQUE NOT NULL, main_branch TEXT NOT NULL, created_at INTEGER NOT NULL);
             CREATE TABLE IF NOT EXISTS counters(repo_id INTEGER NOT NULL, kind TEXT NOT NULL, next_val INTEGER NOT NULL, PRIMARY KEY(repo_id,kind));
             CREATE TABLE IF NOT EXISTS tasks(id TEXT NOT NULL, repo_id INTEGER NOT NULL, title TEXT NOT NULL, brief TEXT NOT NULL, plan TEXT, state TEXT NOT NULL, created_at INTEGER NOT NULL, PRIMARY KEY(repo_id,id));
             CREATE TABLE IF NOT EXISTS assignments(id TEXT NOT NULL, repo_id INTEGER NOT NULL, task_id TEXT NOT NULL, worktree_path TEXT NOT NULL, branch TEXT NOT NULL, state TEXT NOT NULL, created_at INTEGER NOT NULL, PRIMARY KEY(repo_id,id));
             CREATE TABLE IF NOT EXISTS claims(id INTEGER PRIMARY KEY, repo_id INTEGER NOT NULL, assignment_id TEXT NOT NULL, file_path TEXT NOT NULL, item_name TEXT, created_at INTEGER NOT NULL);
             CREATE TABLE IF NOT EXISTS changesets(id TEXT NOT NULL, repo_id INTEGER NOT NULL, assignment_id TEXT NOT NULL, task_id TEXT NOT NULL, worktree_path TEXT NOT NULL, branch TEXT NOT NULL, base_commit TEXT NOT NULL, head_commit TEXT NOT NULL, files_json TEXT NOT NULL, items_json TEXT NOT NULL, claimed_vs_actual_json TEXT NOT NULL, status TEXT NOT NULL, reasons_json TEXT NOT NULL, created_at INTEGER NOT NULL, PRIMARY KEY(repo_id,id));
             CREATE TABLE IF NOT EXISTS escalations(id TEXT NOT NULL, repo_id INTEGER NOT NULL, task_id TEXT NOT NULL, question TEXT NOT NULL, options_json TEXT NOT NULL, recommendation TEXT NOT NULL, answer TEXT, resolved INTEGER NOT NULL DEFAULT 0, created_at INTEGER NOT NULL, PRIMARY KEY(repo_id,id));
             CREATE TABLE IF NOT EXISTS decisions(id TEXT NOT NULL, repo_id INTEGER NOT NULL, text TEXT NOT NULL, revoked INTEGER NOT NULL DEFAULT 0, created_at INTEGER NOT NULL, PRIMARY KEY(repo_id,id));
             CREATE TABLE IF NOT EXISTS events(seq INTEGER PRIMARY KEY AUTOINCREMENT, repo_id INTEGER NOT NULL, kind TEXT NOT NULL, payload_json TEXT NOT NULL, created_at INTEGER NOT NULL);
             CREATE TABLE IF NOT EXISTS gate_runs(id INTEGER PRIMARY KEY, repo_id INTEGER NOT NULL, changeset_id TEXT NOT NULL, tier INTEGER NOT NULL, command TEXT NOT NULL, passed INTEGER NOT NULL, output TEXT NOT NULL, created_at INTEGER NOT NULL);
             CREATE TABLE IF NOT EXISTS leases(repo_id INTEGER PRIMARY KEY, holder TEXT NOT NULL, acquired_at INTEGER NOT NULL);",
        )?;
        // Phase 2.1 additive migrations: v1 homes gain the new columns in place.
        self.add_column_if_missing("changesets", "version", "INTEGER NOT NULL DEFAULT 1")?;
        self.add_column_if_missing("changesets", "superseded", "INTEGER NOT NULL DEFAULT 0")?;
        self.add_column_if_missing("changesets", "superseded_by", "TEXT")?;
        self.add_column_if_missing("gate_runs", "tree_hash", "TEXT")?;
        self.add_column_if_missing("gate_runs", "main_head", "TEXT")?;
        self.add_column_if_missing("gate_runs", "participants_json", "TEXT")?;
        Ok(())
    }
    fn add_column_if_missing(&self, table: &str, column: &str, decl: &str) -> Result<()> {
        let mut st = self.conn.prepare(&format!("PRAGMA table_info({table})"))?;
        let cols = st.query_map([], |r| r.get::<_, String>(1))?.collect::<std::result::Result<Vec<_>, _>>()?;
        if !cols.iter().any(|c| c == column) {
            self.conn.execute_batch(&format!("ALTER TABLE {table} ADD COLUMN {column} {decl}"))?;
        }
        Ok(())
    }
    pub fn emit(&self, repo_id: i64, kind: &str, payload: serde_json::Value) -> Result<i64> {
        self.conn.execute("INSERT INTO events(repo_id,kind,payload_json,created_at) VALUES(?1,?2,?3,?4)", params![repo_id, kind, payload.to_string(), now_secs()])?;
        Ok(self.conn.last_insert_rowid())
    }
    fn next_id(&self, repo_id: i64, kind: &str, prefix: &str) -> Result<String> {
        self.conn.execute("INSERT INTO counters(repo_id,kind,next_val) VALUES(?1,?2,1) ON CONFLICT(repo_id,kind) DO UPDATE SET next_val=next_val+1", params![repo_id, kind])?;
        let n: i64 = self.conn.query_row("SELECT next_val FROM counters WHERE repo_id=?1 AND kind=?2", params![repo_id, kind], |r| r.get(0))?;
        Ok(format!("{prefix}-{n}"))
    }
    // ---- repos ----
    pub fn register_repo(&self, path: &str, main_branch: &str) -> Result<RepoRow> {
        self.conn.execute("INSERT INTO repos(path,main_branch,created_at) VALUES(?1,?2,?3) ON CONFLICT(path) DO UPDATE SET main_branch=excluded.main_branch", params![path, main_branch, now_secs()])?;
        let repo = self.repo_by_path(path)?.context("repo missing after register")?;
        Ok(repo)
    }
    pub fn repo_by_path(&self, path: &str) -> Result<Option<RepoRow>> {
        let mut st = self.conn.prepare("SELECT id,path,main_branch,created_at FROM repos WHERE path=?1")?;
        let mut rows = st.query_map(params![path], |r| Ok(RepoRow { id: r.get(0)?, path: r.get(1)?, main_branch: r.get(2)?, created_at: r.get(3)? }))?;
        Ok(rows.next().transpose()?)
    }
    #[cfg(test)]
    pub fn list_repos(&self) -> Result<Vec<RepoRow>> {
        let mut st = self.conn.prepare("SELECT id,path,main_branch,created_at FROM repos ORDER BY id")?;
        let rows = st.query_map([], |r| Ok(RepoRow { id: r.get(0)?, path: r.get(1)?, main_branch: r.get(2)?, created_at: r.get(3)? }))?;
        Ok(rows.collect::<std::result::Result<Vec<_>, _>>()?)
    }
    // ---- tasks ----
    pub fn create_task(&self, repo_id: i64, title: &str, brief: &str) -> Result<TaskRow> {
        let id = self.next_id(repo_id, "task", "T")?;
        self.conn.execute("INSERT INTO tasks(id,repo_id,title,brief,plan,state,created_at) VALUES(?1,?2,?3,?4,NULL,'queued',?5)", params![id, repo_id, title, brief, now_secs()])?;
        self.emit(repo_id, "task_created", serde_json::json!({"task_id": id, "title": title}))?;
        Ok(self.get_task(repo_id, &id)?.unwrap())
    }
    pub fn get_task(&self, repo_id: i64, id: &str) -> Result<Option<TaskRow>> {
        let mut st = self.conn.prepare("SELECT id,repo_id,title,brief,plan,state,created_at FROM tasks WHERE repo_id=?1 AND id=?2")?;
        let mut rows = st.query_map(params![repo_id, id], |r| Ok(TaskRow { id: r.get(0)?, repo_id: r.get(1)?, title: r.get(2)?, brief: r.get(3)?, plan: r.get(4)?, state: r.get(5)?, created_at: r.get(6)? }))?;
        Ok(rows.next().transpose()?)
    }
    pub fn list_tasks(&self, repo_id: i64) -> Result<Vec<TaskRow>> {
        let mut st = self.conn.prepare("SELECT id,repo_id,title,brief,plan,state,created_at FROM tasks WHERE repo_id=?1 ORDER BY created_at,id")?;
        let rows = st.query_map(params![repo_id], |r| Ok(TaskRow { id: r.get(0)?, repo_id: r.get(1)?, title: r.get(2)?, brief: r.get(3)?, plan: r.get(4)?, state: r.get(5)?, created_at: r.get(6)? }))?;
        Ok(rows.collect::<std::result::Result<Vec<_>, _>>()?)
    }
    pub fn set_task_state(&self, repo_id: i64, id: &str, state: &str) -> Result<()> {
        self.conn.execute("UPDATE tasks SET state=?1 WHERE repo_id=?2 AND id=?3", params![state, repo_id, id])?;
        self.emit(repo_id, "task_state", serde_json::json!({"task_id": id, "state": state}))?;
        Ok(())
    }
    pub fn save_plan(&self, repo_id: i64, id: &str, plan: &str) -> Result<()> {
        self.conn.execute("UPDATE tasks SET plan=?1, state='planned' WHERE repo_id=?2 AND id=?3", params![plan, repo_id, id])?;
        self.emit(repo_id, "task_plan_saved", serde_json::json!({"task_id": id}))?;
        Ok(())
    }
    // ---- assignments ----
    pub fn create_assignment(&self, repo_id: i64, task_id: &str, worktree_path: &str, branch: &str) -> Result<AssignmentRow> {
        let id = self.next_id(repo_id, "assignment", "A")?;
        self.conn.execute("INSERT INTO assignments(id,repo_id,task_id,worktree_path,branch,state,created_at) VALUES(?1,?2,?3,?4,?5,'open',?6)", params![id, repo_id, task_id, worktree_path, branch, now_secs()])?;
        self.emit(repo_id, "assignment_created", serde_json::json!({"assignment_id": id, "task_id": task_id, "worktree": worktree_path, "branch": branch}))?;
        Ok(self.get_assignment(repo_id, &id)?.unwrap())
    }
    pub fn get_assignment(&self, repo_id: i64, id: &str) -> Result<Option<AssignmentRow>> {
        let mut st = self.conn.prepare("SELECT id,repo_id,task_id,worktree_path,branch,state,created_at FROM assignments WHERE repo_id=?1 AND id=?2")?;
        let mut rows = st.query_map(params![repo_id, id], |r| Ok(AssignmentRow { id: r.get(0)?, repo_id: r.get(1)?, task_id: r.get(2)?, worktree_path: r.get(3)?, branch: r.get(4)?, state: r.get(5)?, created_at: r.get(6)? }))?;
        Ok(rows.next().transpose()?)
    }
    pub fn list_assignments(&self, repo_id: i64) -> Result<Vec<AssignmentRow>> {
        let mut st = self.conn.prepare("SELECT id,repo_id,task_id,worktree_path,branch,state,created_at FROM assignments WHERE repo_id=?1 ORDER BY created_at,id")?;
        let rows = st.query_map(params![repo_id], |r| Ok(AssignmentRow { id: r.get(0)?, repo_id: r.get(1)?, task_id: r.get(2)?, worktree_path: r.get(3)?, branch: r.get(4)?, state: r.get(5)?, created_at: r.get(6)? }))?;
        Ok(rows.collect::<std::result::Result<Vec<_>, _>>()?)
    }
    pub fn close_assignment(&self, repo_id: i64, id: &str) -> Result<()> {
        self.conn.execute("UPDATE assignments SET state='closed' WHERE repo_id=?1 AND id=?2", params![repo_id, id])?;
        self.conn.execute("DELETE FROM claims WHERE repo_id=?1 AND assignment_id=?2", params![repo_id, id])?;
        self.emit(repo_id, "assignment_closed", serde_json::json!({"assignment_id": id}))?;
        Ok(())
    }
    // ---- claims ----
    pub fn claim_collisions(&self, repo_id: i64, assignment_id: &str, nodes: &[ClaimNode]) -> Result<Vec<serde_json::Value>> {
        let existing = self.list_claims(repo_id)?;
        let open: std::collections::HashSet<String> = self.list_assignments(repo_id)?.into_iter().filter(|a| a.state == "open").map(|a| a.id).collect();
        let mut out = Vec::new();
        for n in nodes {
            for c in &existing {
                if c.assignment_id == assignment_id || !open.contains(&c.assignment_id) { continue; }
                if c.file_path != n.file { continue; }
                let hit = c.item_name.is_none() || n.item.is_none() || c.item_name == n.item;
                if hit {
                    out.push(serde_json::json!({"assignment_id": c.assignment_id, "holding_assignment": c.assignment_id, "file": c.file_path, "item": n.item, "held_item": c.item_name, "requested_item": n.item}));
                }
            }
        }
        Ok(out)
    }
    fn collisions_for_node(collisions: &[serde_json::Value], node: &ClaimNode) -> bool {
        collisions.iter().any(|c| c.get("file").and_then(|v| v.as_str()) == Some(node.file.as_str()) && c.get("item") == Some(&serde_json::to_value(&node.item).unwrap_or(serde_json::Value::Null)))
    }
    /// Partial grants (F3): uncontested nodes are granted, contested ones are
    /// refused by name. `granted` (all-or-nothing) is derived by callers as
    /// `refused.is_empty()`.
    pub fn acquire_claims_partial(&self, repo_id: i64, assignment_id: &str, nodes: &[ClaimNode]) -> Result<PartialClaimResult> {
        let cols = self.claim_collisions(repo_id, assignment_id, nodes)?;
        let already = self.claims_for(repo_id, assignment_id)?;
        let mut granted_nodes = Vec::new();
        for n in nodes {
            if Self::collisions_for_node(&cols, n) { continue; }
            if !granted_nodes.contains(n) { granted_nodes.push(n.clone()); }
            if !already.contains(n) {
                self.conn.execute("INSERT INTO claims(repo_id,assignment_id,file_path,item_name,created_at) VALUES(?1,?2,?3,?4,?5)", params![repo_id, assignment_id, n.file, n.item, now_secs()])?;
            }
        }
        if !granted_nodes.is_empty() {
            self.emit(repo_id, "claim_granted", serde_json::json!({"assignment_id": assignment_id, "nodes": granted_nodes}))?;
        }
        if !cols.is_empty() {
            self.emit(repo_id, "claim_conflict", serde_json::json!({"assignment_id": assignment_id, "collisions": cols}))?;
        }
        Ok(PartialClaimResult { granted_nodes, refused: cols })
    }
    /// Back-compat wrapper: Ok only when every requested node was granted.
    /// Uncontested nodes are still granted (partial-grant semantics).
    #[allow(dead_code)]
    pub fn acquire_claims(&self, repo_id: i64, assignment_id: &str, nodes: &[ClaimNode]) -> Result<Result<(), Vec<serde_json::Value>>> {
        let r = self.acquire_claims_partial(repo_id, assignment_id, nodes)?;
        if r.refused.is_empty() { Ok(Ok(())) } else { Ok(Err(r.refused)) }
    }
    pub fn list_claims(&self, repo_id: i64) -> Result<Vec<ClaimRow>> {
        let mut st = self.conn.prepare("SELECT id,repo_id,assignment_id,file_path,item_name,created_at FROM claims WHERE repo_id=?1 ORDER BY id")?;
        let rows = st.query_map(params![repo_id], |r| Ok(ClaimRow { id: r.get(0)?, repo_id: r.get(1)?, assignment_id: r.get(2)?, file_path: r.get(3)?, item_name: r.get(4)?, created_at: r.get(5)? }))?;
        Ok(rows.collect::<std::result::Result<Vec<_>, _>>()?)
    }
    pub fn claims_for(&self, repo_id: i64, assignment_id: &str) -> Result<Vec<ClaimNode>> {
        Ok(self.list_claims(repo_id)?.into_iter().filter(|c| c.assignment_id == assignment_id).map(|c| ClaimNode { file: c.file_path, item: c.item_name }).collect())
    }
    /// Per-node release (F3): `None` (or an empty list) releases all of the
    /// assignment's claims; a non-empty list releases exactly those nodes.
    pub fn release_claims_nodes(&self, repo_id: i64, assignment_id: &str, nodes: Option<&[ClaimNode]>) -> Result<()> {
        match nodes {
            Some(list) if !list.is_empty() => {
                for n in list {
                    match &n.item {
                        Some(item) => { self.conn.execute("DELETE FROM claims WHERE repo_id=?1 AND assignment_id=?2 AND file_path=?3 AND item_name=?4", params![repo_id, assignment_id, n.file, item])?; }
                        None => { self.conn.execute("DELETE FROM claims WHERE repo_id=?1 AND assignment_id=?2 AND file_path=?3 AND item_name IS NULL", params![repo_id, assignment_id, n.file])?; }
                    }
                }
                self.emit(repo_id, "claim_released", serde_json::json!({"assignment_id": assignment_id, "nodes": list}))?;
            }
            _ => {
                self.conn.execute("DELETE FROM claims WHERE repo_id=?1 AND assignment_id=?2", params![repo_id, assignment_id])?;
                self.emit(repo_id, "claim_released", serde_json::json!({"assignment_id": assignment_id}))?;
            }
        }
        Ok(())
    }
    // ---- changesets ----
    pub fn create_changeset(&self, repo_id: i64, assignment_id: &str, task_id: &str, worktree_path: &str, branch: &str, base: &str, head: &str, files: &serde_json::Value, items: &serde_json::Value, cva: &serde_json::Value, status: &str, reasons: &serde_json::Value) -> Result<ChangesetRow> {
        let id = self.next_id(repo_id, "changeset", "C")?;
        self.conn.execute("INSERT INTO changesets(id,repo_id,assignment_id,task_id,worktree_path,branch,base_commit,head_commit,files_json,items_json,claimed_vs_actual_json,status,reasons_json,created_at,version,superseded,superseded_by) VALUES(?1,?2,?3,?4,?5,?6,?7,?8,?9,?10,?11,?12,?13,?14,1,0,NULL)", params![id, repo_id, assignment_id, task_id, worktree_path, branch, base, head, files.to_string(), items.to_string(), cva.to_string(), status, reasons.to_string(), now_secs()])?;
        self.emit(repo_id, "changeset_registered", serde_json::json!({"changeset_id": id, "assignment_id": assignment_id, "task_id": task_id, "status": status}))?;
        Ok(self.get_changeset(repo_id, &id)?.unwrap())
    }
    fn changeset_from_row(r: &rusqlite::Row) -> rusqlite::Result<ChangesetRow> {
        Ok(ChangesetRow { id: r.get(0)?, repo_id: r.get(1)?, assignment_id: r.get(2)?, task_id: r.get(3)?, worktree_path: r.get(4)?, branch: r.get(5)?, base_commit: r.get(6)?, head_commit: r.get(7)?, files_json: r.get(8)?, items_json: r.get(9)?, claimed_vs_actual_json: r.get(10)?, status: r.get(11)?, reasons_json: r.get(12)?, created_at: r.get(13)?, version: r.get(14)?, superseded: r.get::<_, i64>(15)? != 0, superseded_by: r.get(16)? })
    }
    const CHANGESET_COLS: &'static str = "id,repo_id,assignment_id,task_id,worktree_path,branch,base_commit,head_commit,files_json,items_json,claimed_vs_actual_json,status,reasons_json,created_at,version,superseded,superseded_by";
    pub fn get_changeset(&self, repo_id: i64, id: &str) -> Result<Option<ChangesetRow>> {
        let mut st = self.conn.prepare(&format!("SELECT {} FROM changesets WHERE repo_id=?1 AND id=?2", Self::CHANGESET_COLS))?;
        let mut rows = st.query_map(params![repo_id, id], Self::changeset_from_row)?;
        Ok(rows.next().transpose()?)
    }
    pub fn list_changesets(&self, repo_id: i64) -> Result<Vec<ChangesetRow>> {
        let mut st = self.conn.prepare(&format!("SELECT {} FROM changesets WHERE repo_id=?1 ORDER BY created_at,id", Self::CHANGESET_COLS))?;
        let rows = st.query_map(params![repo_id], Self::changeset_from_row)?;
        Ok(rows.collect::<std::result::Result<Vec<_>, _>>()?)
    }
    pub fn update_changeset_status(&self, repo_id: i64, id: &str, status: &str, reasons: &serde_json::Value) -> Result<()> {
        self.conn.execute("UPDATE changesets SET status=?1,reasons_json=?2 WHERE repo_id=?3 AND id=?4", params![status, reasons.to_string(), repo_id, id])?;
        self.emit(repo_id, "changeset_status", serde_json::json!({"changeset_id": id, "status": status, "reasons": reasons}))?;
        Ok(())
    }
    /// Store a recomputed live snapshot (files/items/CVA/head/base). Silent
    /// unless `bump_version` (explicit refresh), which is the audible path.
    pub fn update_changeset_snapshot(&self, repo_id: i64, id: &str, base: &str, head: &str, files: &serde_json::Value, items: &serde_json::Value, cva: &serde_json::Value, bump_version: bool) -> Result<i64> {
        if bump_version {
            self.conn.execute("UPDATE changesets SET base_commit=?1,head_commit=?2,files_json=?3,items_json=?4,claimed_vs_actual_json=?5,version=version+1 WHERE repo_id=?6 AND id=?7", params![base, head, files.to_string(), items.to_string(), cva.to_string(), repo_id, id])?;
        } else {
            self.conn.execute("UPDATE changesets SET base_commit=?1,head_commit=?2,files_json=?3,items_json=?4,claimed_vs_actual_json=?5 WHERE repo_id=?6 AND id=?7", params![base, head, files.to_string(), items.to_string(), cva.to_string(), repo_id, id])?;
        }
        Ok(self.get_changeset(repo_id, id)?.map(|c| c.version).unwrap_or(0))
    }
    pub fn mark_superseded(&self, repo_id: i64, id: &str, superseded_by: Option<&str>) -> Result<()> {
        let reason = match superseded_by { Some(by) => format!("superseded by {by}"), None => "superseded".to_string() };
        self.conn.execute("UPDATE changesets SET superseded=1,superseded_by=?1,status='superseded',reasons_json=?2 WHERE repo_id=?3 AND id=?4", params![superseded_by, serde_json::json!([reason]).to_string(), repo_id, id])?;
        self.emit(repo_id, "changeset_superseded", serde_json::json!({"changeset_id": id, "superseded_by": superseded_by}))?;
        Ok(())
    }
    // ---- escalations / decisions / events / gates / lease ----
    pub fn create_escalation(&self, repo_id: i64, task_id: &str, question: &str, options: &serde_json::Value, recommendation: &str) -> Result<EscalationRow> {
        let id = self.next_id(repo_id, "escalation", "E")?;
        self.conn.execute("INSERT INTO escalations(id,repo_id,task_id,question,options_json,recommendation,answer,resolved,created_at) VALUES(?1,?2,?3,?4,?5,?6,NULL,0,?7)", params![id, repo_id, task_id, question, options.to_string(), recommendation, now_secs()])?;
        self.emit(repo_id, "escalation_created", serde_json::json!({"escalation_id": id, "task_id": task_id}))?;
        Ok(self.get_escalation(repo_id, &id)?.unwrap())
    }
    pub fn get_escalation(&self, repo_id: i64, id: &str) -> Result<Option<EscalationRow>> {
        let mut st = self.conn.prepare("SELECT id,repo_id,task_id,question,options_json,recommendation,answer,resolved,created_at FROM escalations WHERE repo_id=?1 AND id=?2")?;
        let mut rows = st.query_map(params![repo_id, id], |r| Ok(EscalationRow { id: r.get(0)?, repo_id: r.get(1)?, task_id: r.get(2)?, question: r.get(3)?, options_json: r.get(4)?, recommendation: r.get(5)?, answer: r.get(6)?, resolved: r.get::<_, i64>(7)? != 0, created_at: r.get(8)? }))?;
        Ok(rows.next().transpose()?)
    }
    pub fn list_escalations(&self, repo_id: i64) -> Result<Vec<EscalationRow>> {
        let mut st = self.conn.prepare("SELECT id,repo_id,task_id,question,options_json,recommendation,answer,resolved,created_at FROM escalations WHERE repo_id=?1 ORDER BY created_at,id")?;
        let rows = st.query_map(params![repo_id], |r| Ok(EscalationRow { id: r.get(0)?, repo_id: r.get(1)?, task_id: r.get(2)?, question: r.get(3)?, options_json: r.get(4)?, recommendation: r.get(5)?, answer: r.get(6)?, resolved: r.get::<_, i64>(7)? != 0, created_at: r.get(8)? }))?;
        Ok(rows.collect::<std::result::Result<Vec<_>, _>>()?)
    }
    pub fn resolve_escalation(&self, repo_id: i64, id: &str, answer: &str) -> Result<()> {
        self.conn.execute("UPDATE escalations SET answer=?1,resolved=1 WHERE repo_id=?2 AND id=?3", params![answer, repo_id, id])?;
        self.emit(repo_id, "escalation_resolved", serde_json::json!({"escalation_id": id, "answer": answer}))?;
        Ok(())
    }
    pub fn log_decision(&self, repo_id: i64, text: &str) -> Result<DecisionRow> {
        let id = self.next_id(repo_id, "decision", "D")?;
        self.conn.execute("INSERT INTO decisions(id,repo_id,text,revoked,created_at) VALUES(?1,?2,?3,0,?4)", params![id, repo_id, text, now_secs()])?;
        self.emit(repo_id, "decision_logged", serde_json::json!({"decision_id": id, "text": text}))?;
        Ok(self.list_decisions(repo_id)?.into_iter().find(|d| d.id == id).unwrap())
    }
    pub fn list_decisions(&self, repo_id: i64) -> Result<Vec<DecisionRow>> {
        let mut st = self.conn.prepare("SELECT id,repo_id,text,revoked,created_at FROM decisions WHERE repo_id=?1 ORDER BY created_at,id")?;
        let rows = st.query_map(params![repo_id], |r| Ok(DecisionRow { id: r.get(0)?, repo_id: r.get(1)?, text: r.get(2)?, revoked: r.get::<_, i64>(3)? != 0, created_at: r.get(4)? }))?;
        Ok(rows.collect::<std::result::Result<Vec<_>, _>>()?)
    }
    pub fn revoke_decision(&self, repo_id: i64, id: &str) -> Result<()> {
        self.conn.execute("UPDATE decisions SET revoked=1 WHERE repo_id=?1 AND id=?2", params![repo_id, id])?;
        self.emit(repo_id, "decision_revoked", serde_json::json!({"decision_id": id}))?;
        Ok(())
    }
    pub fn events_since(&self, repo_id: i64, cursor: i64) -> Result<Vec<EventRow>> {
        let mut st = self.conn.prepare("SELECT seq,repo_id,kind,payload_json,created_at FROM events WHERE repo_id=?1 AND seq>?2 ORDER BY seq")?;
        let rows = st.query_map(params![repo_id, cursor], |r| Ok(EventRow { seq: r.get(0)?, repo_id: r.get(1)?, kind: r.get(2)?, payload_json: r.get(3)?, created_at: r.get(4)? }))?;
        Ok(rows.collect::<std::result::Result<Vec<_>, _>>()?)
    }
    #[allow(dead_code)]
    pub fn record_gate(&self, repo_id: i64, changeset_id: &str, tier: i64, command: &str, passed: bool, output: &str) -> Result<()> {
        self.record_gate_with_proof(repo_id, changeset_id, tier, command, passed, output, None, None, None)
    }
    /// Record a gate verdict bound to its proof context (F6): the tree it ran
    /// on, the main head at the time, and every participant snapshot in it.
    pub fn record_gate_with_proof(&self, repo_id: i64, changeset_id: &str, tier: i64, command: &str, passed: bool, output: &str, tree_hash: Option<&str>, main_head: Option<&str>, participants: Option<&serde_json::Value>) -> Result<()> {
        self.conn.execute("INSERT INTO gate_runs(repo_id,changeset_id,tier,command,passed,output,created_at,tree_hash,main_head,participants_json) VALUES(?1,?2,?3,?4,?5,?6,?7,?8,?9,?10)", params![repo_id, changeset_id, tier, command, passed as i64, output, now_secs(), tree_hash, main_head, participants.map(|p| p.to_string())])?;
        self.emit(repo_id, "gate_run", serde_json::json!({"changeset_id": changeset_id, "tier": tier, "command": command, "passed": passed, "tree_hash": tree_hash, "main_head": main_head, "participants": participants}))?;
        Ok(())
    }
    pub fn gate_runs_for(&self, repo_id: i64, changeset_id: &str) -> Result<Vec<GateRunRow>> {
        let mut st = self.conn.prepare("SELECT id,repo_id,changeset_id,tier,command,passed,output,created_at,tree_hash,main_head,participants_json FROM gate_runs WHERE repo_id=?1 AND changeset_id=?2 ORDER BY id")?;
        let rows = st.query_map(params![repo_id, changeset_id], |r| Ok(GateRunRow { id: r.get(0)?, repo_id: r.get(1)?, changeset_id: r.get(2)?, tier: r.get(3)?, command: r.get(4)?, passed: r.get::<_, i64>(5)? != 0, output: r.get(6)?, created_at: r.get(7)?, tree_hash: r.get(8)?, main_head: r.get(9)?, participants_json: r.get(10)? }))?;
        Ok(rows.collect::<std::result::Result<Vec<_>, _>>()?)
    }
    pub fn lease_holder(&self, repo_id: i64) -> Result<Option<(String, i64)>> {
        let mut st = self.conn.prepare("SELECT holder,acquired_at FROM leases WHERE repo_id=?1")?;
        let mut rows = st.query_map(params![repo_id], |r| Ok((r.get::<_, String>(0)?, r.get::<_, i64>(1)?)))?;
        Ok(rows.next().transpose()?)
    }
    pub fn lease_acquire(&self, repo_id: i64, holder: &str) -> Result<Result<(), String>> {
        if let Some((h, _)) = self.lease_holder(repo_id)? {
            if h != holder { return Ok(Err(h)); }
            return Ok(Ok(()));
        }
        self.conn.execute("INSERT INTO leases(repo_id,holder,acquired_at) VALUES(?1,?2,?3)", params![repo_id, holder, now_secs()])?;
        self.emit(repo_id, "lease_acquired", serde_json::json!({"holder": holder}))?;
        Ok(Ok(()))
    }
    pub fn lease_release(&self, repo_id: i64, holder: &str) -> Result<()> {
        self.conn.execute("DELETE FROM leases WHERE repo_id=?1 AND holder=?2", params![repo_id, holder])?;
        self.emit(repo_id, "lease_released", serde_json::json!({"holder": holder}))?;
        Ok(())
    }
}
