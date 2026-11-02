//! Opt-in dependency scheduling. Durable attempts are reserved before any launch.
use anyhow::{bail, Context, Result};
use rusqlite::params;
use serde::{Deserialize, Serialize};
use serde_json::{json, Value};
use std::path::{Path, PathBuf};

use crate::{
    agent,
    config::TowerConfig,
    engine, git,
    store::{ClaimNode, Store},
};

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct TaskSpec {
    #[serde(default)]
    pub depends_on: Vec<String>,
    #[serde(default)]
    pub nodes: Vec<ClaimNode>,
    #[serde(default)]
    pub assumptions: Vec<String>,
    #[serde(default)]
    pub acceptance_tests: Vec<String>,
    #[serde(default = "one")]
    pub max_attempts: u32,
    #[serde(default)]
    pub agent: Option<agent::AgentSpec>,
}
fn one() -> u32 {
    1
}
impl Default for TaskSpec {
    fn default() -> Self {
        Self {
            depends_on: vec![],
            nodes: vec![],
            assumptions: vec![],
            acceptance_tests: vec![],
            max_attempts: 1,
            agent: None,
        }
    }
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Run {
    pub id: i64,
    pub task_id: String,
    pub assignment_id: String,
    pub attempt: u32,
    pub state: String,
    pub run_dir: String,
    pub changeset_id: Option<String>,
    pub detail: String,
}

pub fn migrate(store: &Store) -> Result<()> {
    store.conn.execute_batch("CREATE TABLE IF NOT EXISTS schedule_specs(
      repo_id INTEGER NOT NULL,task_id TEXT NOT NULL,spec_json TEXT NOT NULL,
      PRIMARY KEY(repo_id,task_id));
      CREATE TABLE IF NOT EXISTS agent_runs(
      id INTEGER PRIMARY KEY,repo_id INTEGER NOT NULL,task_id TEXT NOT NULL,
      assignment_id TEXT NOT NULL,attempt INTEGER NOT NULL,state TEXT NOT NULL,
      run_dir TEXT NOT NULL,changeset_id TEXT,detail TEXT NOT NULL DEFAULT '',launch_json TEXT,
      UNIQUE(repo_id,task_id,attempt));
      CREATE UNIQUE INDEX IF NOT EXISTS one_active_scheduled_agent_per_task_v03 ON agent_runs(repo_id,task_id)
      WHERE state IN ('preparing','starting','running','cancelling','uncertain');")?;
    let columns: Vec<String> = store
        .conn
        .prepare("PRAGMA table_info(agent_runs)")?
        .query_map([], |row| row.get(1))?
        .collect::<std::result::Result<_, _>>()?;
    if !columns.iter().any(|column| column == "launch_json") {
        store
            .conn
            .execute_batch("ALTER TABLE agent_runs ADD COLUMN launch_json TEXT")?;
    }
    Ok(())
}

pub fn spec(store: &Store, rid: i64, task: &str) -> Result<Option<TaskSpec>> {
    let mut st = store
        .conn
        .prepare("SELECT spec_json FROM schedule_specs WHERE repo_id=?1 AND task_id=?2")?;
    let raw = st
        .query_map(params![rid, task], |r| r.get::<_, String>(0))?
        .next()
        .transpose()?;
    raw.map(|s| Ok(serde_json::from_str(&s)?)).transpose()
}

pub fn configure(store: &Store, rid: i64, task: &str, value: &TaskSpec) -> Result<Value> {
    store.atomic(|| {
        let row = store.get_task(rid,task)?.with_context(|| format!("unknown task {task}"))?;
        if !matches!(row.state.as_str(), "queued"|"planned"|"blocked") { bail!("task {task} is {}, cannot change its schedule",row.state); }
        if !(1..=5).contains(&value.max_attempts) { bail!("max_attempts must be between 1 and 5"); }
        if let Some(a) = &value.agent { validate_agent(a)?; }
        for n in &value.nodes { validate_node(n)?; }
        for d in &value.depends_on {
            if d == task { bail!("task cannot depend on itself"); }
            store.get_task(rid,d)?.with_context(|| format!("unknown dependency {d}"))?;
            let mut pending = vec![d.clone()];
            let mut seen = std::collections::HashSet::new();
            while let Some(id) = pending.pop() {
                if id == task { bail!("dependency cycle involving {task}"); }
                if seen.insert(id.clone()) { if let Some(s) = spec(store,rid,&id)? { pending.extend(s.depends_on); } }
            }
        }
        store.conn.execute("INSERT INTO schedule_specs(repo_id,task_id,spec_json) VALUES(?1,?2,?3)
          ON CONFLICT(repo_id,task_id) DO UPDATE SET spec_json=excluded.spec_json",params![rid,task,serde_json::to_string(value)?])?;
        store.emit(rid,"task_scheduled",json!({"task_id":task,"depends_on":value.depends_on,"nodes":value.nodes,"max_attempts":value.max_attempts}))?;
        Ok(json!({"task_id":task,"schedule":value}))
    })
}

fn validate_agent(a: &agent::AgentSpec) -> Result<()> {
    if a.program.trim().is_empty() || a.timeout_secs == 0 || a.timeout_secs > 86400 {
        bail!("agent needs a program and timeout_secs between 1 and 86400");
    }
    Ok(())
}
fn validate_node(n: &ClaimNode) -> Result<()> {
    let p = Path::new(&n.file);
    if n.file.is_empty()
        || p.is_absolute()
        || p.components()
            .any(|c| !matches!(c, std::path::Component::Normal(_)))
        || n.item.as_ref().is_some_and(|s| s.is_empty())
    {
        bail!("claim scope must be a repo-relative file and optional nonempty item");
    }
    Ok(())
}

pub fn runs(store: &Store, rid: i64) -> Result<Vec<Run>> {
    let mut st = store.conn.prepare("SELECT id,task_id,assignment_id,attempt,state,run_dir,changeset_id,detail FROM agent_runs WHERE repo_id=?1 ORDER BY id")?;
    let rows = st.query_map([rid], |r| {
        Ok(Run {
            id: r.get(0)?,
            task_id: r.get(1)?,
            assignment_id: r.get(2)?,
            attempt: r.get(3)?,
            state: r.get(4)?,
            run_dir: r.get(5)?,
            changeset_id: r.get(6)?,
            detail: r.get(7)?,
        })
    })?;
    Ok(rows.collect::<std::result::Result<_, _>>()?)
}
fn update(store: &Store, rid: i64, run: &Run, state: &str, detail: &str) -> Result<()> {
    store.conn.execute(
        "UPDATE agent_runs SET state=?1,detail=?2 WHERE repo_id=?3 AND id=?4",
        params![state, detail, rid, run.id],
    )?;
    if run.state != state {
        store.emit(rid,"agent_state",json!({"run_id":run.id,"task_id":run.task_id,"assignment_id":run.assignment_id,"state":state,"detail":detail}))?;
    }
    Ok(())
}

/// Readiness includes human-owned assignments, claims, failed dependencies and pending handoffs.
pub fn ready(store: &Store, rid: i64, cfg: &TowerConfig) -> Result<Value> {
    let assignments = store.list_assignments(rid)?;
    let all_runs = runs(store, rid)?;
    let reconciliations = crate::reconcile::list(store, rid)?;
    let mut out = Vec::new();
    for t in store.list_tasks(rid)? {
        let Some(s) = spec(store, rid, &t.id)? else {
            continue;
        };
        let mut reasons = Vec::new();
        if !matches!(t.state.as_str(), "queued" | "planned" | "blocked") {
            reasons.push(format!("task is {}", t.state));
        }
        for dep in &s.depends_on {
            let state = store
                .get_task(rid, dep)?
                .map(|t| t.state)
                .unwrap_or_else(|| "missing".into());
            if state != "landed" {
                reasons.push(format!("dependency {dep} is {state}"));
            }
        }
        if assignments
            .iter()
            .any(|a| a.task_id == t.id && a.state == "open")
        {
            reasons.push("task already has an open assignment".into());
        }
        if all_runs.iter().filter(|r| r.task_id == t.id).count() >= s.max_attempts as usize {
            reasons.push("attempt budget exhausted".into());
        }
        let designated = reconciliations
            .iter()
            .any(|reconciliation| reconciliation.task_id == t.id);
        if !designated && !store.claim_collisions(rid, "", &s.nodes)?.is_empty() {
            reasons.push("scope held by another assignment".into());
        }
        if !crate::reconcile::scheduler_task_ready(store, rid, &t.id)? {
            reasons.push("reconciliation handoff pending".into());
        }
        if s.agent.as_ref().or(cfg.scheduler.agent.as_ref()).is_none() {
            reasons.push("no agent executable configured".into());
        }
        out.push(json!({"task_id":t.id,"ready":reasons.is_empty(),"reasons":reasons,"depends_on":s.depends_on,"nodes":s.nodes}));
    }
    Ok(json!(out))
}

pub fn status(store: &Store, rid: i64, cfg: &TowerConfig) -> Result<Value> {
    Ok(json!({"tasks":ready(store,rid,cfg)?,"runs":runs(store,rid)?}))
}

#[derive(Debug, Clone, Serialize, Deserialize)]
struct Launch {
    agent: agent::AgentSpec,
    prompt: String,
}

fn active(state: &str) -> bool {
    matches!(
        state,
        "preparing" | "starting" | "running" | "cancelling" | "uncertain"
    )
}

/// Existing assignment-close/task-kill paths must not release a live scheduled
/// worker's ownership. Only durable stopped receipts prove quiescence.
pub fn assignment_is_quiescent(store: &Store, rid: i64, assignment_id: &str) -> Result<bool> {
    let Some(assignment) = store.get_assignment(rid, assignment_id)? else {
        return Ok(true);
    };
    for run in runs(store, rid)?
        .into_iter()
        .filter(|run| run.assignment_id == assignment_id)
    {
        let path = Path::new(&run.run_dir);
        if agent::quiescent(path, Path::new(&assignment.worktree_path)).unwrap_or(false) {
            continue;
        }
        let cancelled_before_launch = run.state == "cancelled"
            && !path.join("request.json").exists()
            && std::fs::read_to_string(path.join("cancel")).ok().as_deref()
                == Some("cancelled before launch\n")
            && matches!(agent::observe(path), Ok(agent::AgentStatus::Cancelled));
        let preparation_failed = run.state == "review"
            && run.detail.starts_with("worktree preparation failed;")
            && !path.exists();
        if !cancelled_before_launch && !preparation_failed {
            return Ok(false);
        }
    }
    Ok(true)
}

fn require_holder(store: &Store, rid: i64, holder: &str) -> Result<()> {
    match store.lease_holder(rid)? {
        Some((owner, _)) if owner == holder => Ok(()),
        Some((owner, _)) => bail!("orchestrator lease held by {owner}"),
        None => bail!("orchestrator lease was released"),
    }
}

/// Reserve one complete immutable attempt before touching git or launching a process.
fn reserve_task(
    store: &Store,
    rid: i64,
    repo: &Path,
    cfg: &TowerConfig,
    holder: &str,
    task_id: &str,
) -> Result<Option<Run>> {
    store.atomic(|| {
        require_holder(store, rid, holder)?;
        let reconciliation_tasks: std::collections::HashSet<String> = crate::reconcile::list(store,rid)?.into_iter().map(|item|item.task_id).collect();
        let designated=reconciliation_tasks.contains(task_id);
        let occupied=store.list_assignments(rid)?.iter().filter(|assignment|assignment.state=="open" && reconciliation_tasks.contains(&assignment.task_id)==designated).count();
        let slots=if designated {cfg.limits.max_concurrent_reconciliations.unwrap_or(1)} else {cfg.limits.max_concurrent_assignments.unwrap_or(4)};
        if occupied >= slots as usize { return Ok(None); }
        // Re-evaluate after every reservation, including claims granted earlier in this tick.
        if !ready(store, rid, cfg)?.as_array().context("ready rows")?.iter().any(|row| row["task_id"] == task_id && row["ready"] == true) { return Ok(None); }
        let schedule = spec(store, rid, task_id)?.context("schedule missing")?;
        let adapter = schedule.agent.as_ref().or(cfg.scheduler.agent.as_ref()).context("no agent executable configured")?.clone();
        validate_agent(&adapter)?;
        let task = store.get_task(rid, task_id)?.context("task missing")?;
        let assignment = store.create_assignment(rid, task_id, "", "")?;
        let branch = format!("tower/{}-{}", task_id.to_lowercase(), assignment.id.to_lowercase());
        let path = store.home.join("worktrees").join(crate::config::repo_hash(&git::canonical_str(repo))).join(&assignment.id);
        store.conn.execute("UPDATE assignments SET worktree_path=?1,branch=?2 WHERE repo_id=?3 AND id=?4", params![path.to_string_lossy(),branch,rid,assignment.id])?;
        let handoff = crate::reconcile::prepare_dispatch(store, rid, task_id, &assignment.id)?.unwrap_or_default();
        if !designated {
            let claims = store.acquire_claims_partial(rid, &assignment.id, &schedule.nodes)?;
            if !claims.refused.is_empty() { bail!("scope changed while reserving {task_id}"); }
        }
        let prompt = format!("Tower task {}: {}\n\n{}\n\nPlan:\n{}\n\nAssigned scope: {}\nAssignment: {}\n{}\n\nWork only within this authorized task and worktree. Preserve other workers' drafts. Commit completed changes; do not publish, land, change repository visibility or bypass approvals. Poll Tower events and cooperate with reconciliation handoffs; provisional stubs never count as completed behavior. Report unresolved requirements explicitly.\n",task.id,task.title,task.brief,task.plan.unwrap_or_default(),serde_json::to_string(&schedule.nodes)?,assignment.id,handoff);
        let attempt = runs(store,rid)?.iter().filter(|run|run.task_id==task_id).count()+1;
        let run_dir = store.home.join("agents").join(rid.to_string()).join(task_id).join(attempt.to_string());
        let launch = Launch { agent: adapter, prompt };
        store.conn.execute("INSERT INTO agent_runs(repo_id,task_id,assignment_id,attempt,state,run_dir,launch_json) VALUES(?1,?2,?3,?4,'preparing',?5,?6)",params![rid,task_id,assignment.id,attempt,run_dir.to_string_lossy(),serde_json::to_string(&launch)?])?;
        store.set_task_state(rid, task_id, "running")?;
        store.emit(rid,"agent_reserved",json!({"task_id":task_id,"assignment_id":assignment.id,"attempt":attempt}))?;
        Ok(runs(store,rid)?.into_iter().find(|run|run.task_id==task_id && run.attempt==attempt as u32))
    })
}

fn prepare_worktrees(
    store: &Store,
    rid: i64,
    repo: &Path,
    cfg: &TowerConfig,
    holder: &str,
) -> Result<()> {
    for run in runs(store, rid)?
        .into_iter()
        .filter(|run| run.state == "preparing")
    {
        // The durable DB reservation already owns its path/branch and claims. A
        // filesystem fence makes repeated same-holder ticks converge after a crash.
        let fence = store
            .home
            .join("agent-preparations")
            .join(format!("{}-{}", rid, run.id));
        std::fs::create_dir_all(fence.parent().context("preparation directory")?)?;
        let won = match std::fs::create_dir(&fence) {
            Ok(()) => true,
            Err(error) if error.kind() == std::io::ErrorKind::AlreadyExists => false,
            Err(error) => return Err(error.into()),
        };
        let assignment = store
            .get_assignment(rid, &run.assignment_id)?
            .context("assignment missing")?;
        let path = Path::new(&assignment.worktree_path);
        let existing = git::worktree_list(repo)?.into_iter().any(|tree| {
            git::canonical_str(Path::new(&tree.path)) == git::canonical_str(path)
                && tree.branch.as_deref() == Some(&assignment.branch)
        });
        if !won && !existing {
            continue;
        } // In-progress/uncertain preparation never starts twice.
        let result = if existing {
            Ok(())
        } else {
            // Keep cancellation/lease handoff serialized with this short git
            // operation, but commit its reservation in an earlier transaction.
            store.atomic(|| {
                require_holder(store, rid, holder)?;
                let fresh = runs(store, rid)?
                    .into_iter()
                    .find(|r| r.id == run.id)
                    .context("run missing")?;
                if fresh.state != "preparing" {
                    return Ok(());
                }
                if let Some(parent) = path.parent() {
                    std::fs::create_dir_all(parent)?;
                }
                git::run_git(
                    repo,
                    &[
                        "worktree",
                        "add",
                        "-b",
                        &assignment.branch,
                        &assignment.worktree_path,
                        &cfg.repo.main_branch,
                    ],
                )?;
                Ok(())
            })
        };
        store.atomic(|| {
            require_holder(store,rid,holder)?;
            let fresh = runs(store,rid)?.into_iter().find(|r|r.id==run.id).context("run missing")?;
            if fresh.state!="preparing" { return Ok(()); }
            match &result {
                Ok(()) => {
                    update(store,rid,&fresh,"starting","")?;
                    store.emit(rid,"worktree_created",json!({"assignment_id":assignment.id,"task_id":run.task_id,"worktree":assignment.worktree_path,"branch":assignment.branch}))?;
                }
                Err(error) => {
                    // A git operation can leave a branch or partial worktree. Keep
                    // the durable record and ownership for explicit investigation.
                    update(store,rid,&fresh,"review",&format!("worktree preparation failed; preserve and inspect {}: {error:#}",assignment.worktree_path))?;
                    store.set_task_state(rid,&run.task_id,"blocked")?;
                }
            }
            Ok(())
        })?;
    }
    Ok(())
}

fn launch_starting(store: &Store, rid: i64, holder: &str) -> Result<()> {
    for candidate in runs(store, rid)?
        .into_iter()
        .filter(|run| run.state == "starting")
    {
        store.atomic(|| {
            require_holder(store, rid, holder)?;
            let run = runs(store, rid)?
                .into_iter()
                .find(|run| run.id == candidate.id)
                .context("run missing")?;
            if run.state != "starting" {
                return Ok(());
            }
            let assignment = store
                .get_assignment(rid, &run.assignment_id)?
                .context("assignment missing")?;
            let raw: Option<String> = store.conn.query_row(
                "SELECT launch_json FROM agent_runs WHERE repo_id=?1 AND id=?2",
                params![rid, run.id],
                |row| row.get(0),
            )?;
            let Some(raw) = raw else {
                return update(
                    store,
                    rid,
                    &run,
                    "uncertain",
                    "attempt has no frozen launch specification; inspect before recovery",
                );
            };
            let launch: Launch = serde_json::from_str(&raw)?;
            // Cancellation uses this same DB lock. Either it fences an unlaunched
            // directory first, or it requests cancellation from the live owner.
            match agent::launch(
                Path::new(&run.run_dir),
                Path::new(&assignment.worktree_path),
                &launch.agent,
                &launch.prompt,
            ) {
                Ok(state) => apply_receipt(store, rid, &run, &state)?,
                Err(error) => update(
                    store,
                    rid,
                    &run,
                    "uncertain",
                    &format!("launch needs inspection: {error:#}"),
                )?,
            }
            Ok(())
        })?;
    }
    Ok(())
}

/// One bounded coordination iteration. Only explicitly scheduled tasks launch;
/// integration remains an explicit operation after gate and reconciliation proof.
pub fn tick(
    store: &Store,
    rid: i64,
    repo: &Path,
    cfg: &TowerConfig,
    holder: &str,
) -> Result<Value> {
    if holder.trim().is_empty() {
        bail!("coordinator holder cannot be empty");
    }
    store.atomic(|| {
        match store.lease_acquire(rid, holder)? {
            Ok(()) => {}
            Err(owner) => bail!("orchestrator lease held by {owner}"),
        }
        observe_runs(store, rid)?;
        Ok(())
    })?;
    discover_handoffs(store, rid, repo, cfg, holder)?;
    let mut candidates: Vec<String> = ready(store, rid, cfg)?
        .as_array()
        .context("ready rows")?
        .iter()
        .filter(|row| row["ready"] == true)
        .filter_map(|row| row["task_id"].as_str().map(str::to_owned))
        .collect();
    let reconciliation_tasks: std::collections::HashSet<String> =
        crate::reconcile::list(store, rid)?
            .into_iter()
            .map(|item| item.task_id)
            .collect();
    candidates.sort_by_key(|task| !reconciliation_tasks.contains(task));
    for task in candidates {
        if let Err(error) = reserve_task(store, rid, repo, cfg, holder, &task) {
            // Reservations contain no external effects and can safely roll back.
            // One malformed task must not roll back or starve independent work.
            store.atomic(|| {
                require_holder(store, rid, holder)?;
                store.emit(
                    rid,
                    "schedule_error",
                    json!({"task_id":task,"detail":format!("{error:#}")}),
                )?;
                Ok(())
            })?;
        }
    }
    prepare_worktrees(store, rid, repo, cfg, holder)?;
    launch_starting(store, rid, holder)?;
    drain_resources(store, rid, repo, cfg, holder)?;
    verify_finished(store, rid, repo, cfg, holder)?;
    status(store, rid, cfg)
}

/// A configured coordinator can propose handoffs from recorded authored goals;
/// workers still acknowledge safe boundaries and author a stable provisional contract.
fn discover_handoffs(
    store: &Store,
    rid: i64,
    repo: &Path,
    cfg: &TowerConfig,
    holder: &str,
) -> Result<()> {
    if cfg.scheduler.agent.is_none() {
        return Ok(());
    }
    let found = crate::reconcile::discover(store, rid, repo, cfg)?;
    for overlap in found.as_array().context("overlap rows")? {
        let nodes: Vec<ClaimNode> = serde_json::from_value(overlap["nodes"].clone())?;
        let reserved = crate::reconcile::list(store, rid)?.into_iter().any(|r| {
            !matches!(r.state.as_str(), "completed" | "cancelled")
                && r.nodes.iter().any(|a| {
                    nodes.iter().any(|b| {
                        a.file == b.file
                            && (a.item.is_none() || b.item.is_none() || a.item == b.item)
                    })
                })
        });
        if reserved {
            continue;
        }
        let mut intents = Vec::new();
        for id in overlap["assignments"]
            .as_array()
            .context("overlap assignments")?
        {
            let assignment = store
                .get_assignment(rid, id.as_str().context("overlap assignment id")?)?
                .context("assignment missing")?;
            let task = store
                .get_task(rid, &assignment.task_id)?
                .context("task missing")?;
            if task.brief.trim().is_empty() {
                break;
            }
            let schedule = spec(store, rid, &task.id)?.unwrap_or_default();
            intents.push(json!({"assignment_id":assignment.id,"intent":format!("{}\n{}",task.brief,task.plan.unwrap_or_default()),"assumptions":schedule.assumptions,"acceptance_tests":schedule.acceptance_tests}));
        }
        if intents.len() != 2 {
            continue;
        }
        store.atomic(|| {
            require_holder(store,rid,holder)?;
            let reconciliation=crate::reconcile::request(store,rid,serde_json::from_value(json!(intents))?,nodes)?;
            store.emit(rid,"reconciliation_discovered",json!({"reconciliation_id":reconciliation.id,"task_id":reconciliation.task_id,"next":"participants supply acceptance tests, pause only shared edits, and negotiate an authored stable contract if possible"}))?;
            Ok(())
        })?;
    }
    Ok(())
}

fn apply_receipt(store: &Store, rid: i64, run: &Run, state: &agent::AgentStatus) -> Result<()> {
    update(
        store,
        rid,
        run,
        state.state(),
        &serde_json::to_string(state)?,
    )?;
    if state.retryable() {
        let schedule = spec(store, rid, &run.task_id)?.context("schedule missing")?;
        if let Some(reconciliation) = crate::reconcile::list(store, rid)?
            .into_iter()
            .find(|r| r.task_id == run.task_id)
        {
            // A handoff binds both frozen drafts to one owner. Starting another
            // assignment would invalidate that transfer; retain it for recovery.
            crate::reconcile::cancel(store,rid,&reconciliation.id,"reconciliation agent failed; inspect preserved draft, cancel/recover the handoff before another attempt")?;
            store.set_task_state(rid, &run.task_id, "failed")?;
            return Ok(());
        }
        store.close_assignment(rid, &run.assignment_id)?;
        let cancelled = store
            .get_task(rid, &run.task_id)?
            .is_some_and(|task| task.state == "cancelled");
        store.set_task_state(
            rid,
            &run.task_id,
            if cancelled {
                "cancelled"
            } else if run.attempt < schedule.max_attempts {
                "queued"
            } else {
                "failed"
            },
        )?;
    } else if matches!(state, agent::AgentStatus::Cancelled) {
        store.close_assignment(rid, &run.assignment_id)?;
        store.set_task_state(rid, &run.task_id, "cancelled")?;
    }
    Ok(())
}

fn observe_runs(store: &Store, rid: i64) -> Result<()> {
    for run in runs(store, rid)?
        .into_iter()
        .filter(|run| active(&run.state) && run.state != "preparing")
    {
        if run.state == "starting" && !Path::new(&run.run_dir).exists() {
            continue;
        }
        let state = agent::observe(Path::new(&run.run_dir))?;
        apply_receipt(store, rid, &run, &state)?;
    }
    Ok(())
}

fn drain_resources(
    store: &Store,
    rid: i64,
    repo: &Path,
    cfg: &TowerConfig,
    holder: &str,
) -> Result<()> {
    for job in crate::resources::list(store, Some(rid))?
        .into_iter()
        .filter(|job| job.state == "queued")
    {
        store.atomic(|| require_holder(store, rid, holder))?;
        let operation = job.details["operation"].as_str().unwrap_or("");
        let result = match operation {
            "gate" => (|| -> Result<Value> {
                let id = job.details["changeset_id"]
                    .as_str()
                    .context("queued gate missing changeset")?;
                let tier = job.details["tier"]
                    .as_i64()
                    .context("queued gate missing tier")?;
                engine::gate_run_single(store, rid, repo, cfg, id, tier)
            })(),
            "rehearse" => (|| -> Result<Value> {
                let tiers: Vec<i64> = serde_json::from_value(job.details["tiers"].clone())?;
                engine::rehearse(store, rid, repo, cfg, &tiers)
            })(),
            // Landing remains explicit; an unrelated queued operation is never
            // interpreted as permission to publish or integrate code.
            "land" | "landing_candidate" | "reconcile_acceptance" => continue,
            _ => Err(anyhow::anyhow!("unsupported queued resource operation")),
        };
        if let Err(error) = result {
            // Remove a definitively invalid queued request so it cannot pin FIFO
            // forever. Running jobs retain their own guards/recovery semantics.
            let current = crate::resources::list(store, Some(rid))?
                .into_iter()
                .find(|current| current.id == job.id);
            if current.is_some_and(|current| current.state == "queued") {
                crate::resources::cancel(store, rid, job.id)?;
            }
            store.emit(
                rid,
                "queued_operation_error",
                json!({"resource_job_id":job.id,"detail":format!("{error:#}")}),
            )?;
        }
    }
    Ok(())
}

struct VerificationFence(PathBuf);
impl Drop for VerificationFence {
    fn drop(&mut self) {
        let _ = std::fs::remove_dir(&self.0);
    }
}

fn current_gate(
    store: &Store,
    rid: i64,
    repo: &Path,
    cfg: &TowerConfig,
    cid: &str,
    tier: i64,
) -> Result<bool> {
    let gates = store.gate_runs_for(rid, cid)?;
    let Some(gate) = gates.iter().rev().find(|gate| gate.tier == tier) else {
        return Ok(false);
    };
    if !gate.passed {
        return Ok(false);
    }
    let mut snapshots = std::collections::HashMap::new();
    for cs in store.list_changesets(rid)? {
        let path = Path::new(&cs.worktree_path);
        let snapshot = if path.exists() {
            git::snapshot_commit(path)?
        } else {
            cs.head_commit
        };
        snapshots.insert(cs.id, snapshot);
    }
    Ok(engine::gate_proof_current(
        gate,
        &snapshots,
        &engine::main_head(repo, cfg)?,
    ))
}

fn verify_finished(
    store: &Store,
    rid: i64,
    repo: &Path,
    cfg: &TowerConfig,
    holder: &str,
) -> Result<()> {
    for candidate in runs(store, rid)?
        .into_iter()
        .filter(|run| run.state == "succeeded" || run.state == "verifying")
    {
        let path = store
            .home
            .join("agent-verifications")
            .join(format!("{}-{}", rid, candidate.id));
        std::fs::create_dir_all(path.parent().context("verification directory")?)?;
        match std::fs::create_dir(&path) {
            Ok(()) => {}
            Err(error) if error.kind() == std::io::ErrorKind::AlreadyExists => continue,
            Err(error) => return Err(error.into()),
        }
        let _fence = VerificationFence(path);
        let run = store.atomic(|| {
            require_holder(store, rid, holder)?;
            runs(store, rid)?
                .into_iter()
                .find(|run| run.id == candidate.id)
                .context("run missing")
        })?;
        if !matches!(run.state.as_str(), "succeeded" | "verifying") {
            continue;
        }
        let result = (|| -> Result<()> {
            let assignment = store
                .get_assignment(rid, &run.assignment_id)?
                .context("assignment missing")?;
            // Surface git failures instead of treating an unavailable worktree as clean.
            if !git::run_git(
                Path::new(&assignment.worktree_path),
                &["status", "--porcelain"],
            )?
            .trim()
            .is_empty()
            {
                bail!("agent left uncommitted changes; commit and register manually");
            }
            let cid = if let Some(id) = &run.changeset_id {
                id.clone()
            } else {
                store.atomic(|| {
                    require_holder(store,rid,holder)?;
                    let value=engine::register_changeset(store,rid,repo,cfg,&assignment.id,&assignment.worktree_path)?;
                    let id=value["changeset_id"].as_str().context("changeset id")?.to_owned();
                    store.conn.execute("UPDATE agent_runs SET changeset_id=?1,state='verifying' WHERE repo_id=?2 AND id=?3",params![id,rid,run.id])?;
                    Ok(id)
                })?
            };
            for (tier, enabled) in [
                (1, cfg.gates.check.is_some()),
                (2, cfg.gates.test.is_some()),
            ] {
                if enabled && !current_gate(store, rid, repo, cfg, &cid, tier)? {
                    store.atomic(|| require_holder(store, rid, holder))?;
                    let receipt = engine::gate_run_single(store, rid, repo, cfg, &cid, tier)?;
                    if receipt["queued"] == true {
                        return Ok(());
                    }
                    if receipt["passed"] == false {
                        bail!("changeset {cid} failed Tier-{tier} gate; inspect its proof receipt");
                    }
                }
            }
            store.atomic(|| require_holder(store, rid, holder))?;
            crate::reconcile::publish_ready(store, rid, repo, cfg, &run.task_id, &cid)?;
            store.atomic(|| {
                require_holder(store, rid, holder)?;
                if store
                    .get_task(rid, &run.task_id)?
                    .is_some_and(|task| task.state == "cancelled")
                {
                    return Ok(());
                }
                let cs = crate::board::changeset_status_value(store, rid, repo, cfg, &cid)?;
                if cs["status"] != "green" {
                    bail!("changeset {cid} needs review: {}", cs["reasons"]);
                }
                update(
                    store,
                    rid,
                    &run,
                    "review",
                    "committed result verified; explicit integration required",
                )?;
                store.set_task_state(rid, &run.task_id, "integrating")?;
                Ok(())
            })
        })();
        if let Err(error) = result {
            let pending = error
                .to_string()
                .starts_with("acceptance validation queued:");
            store.atomic(|| {
                update(
                    store,
                    rid,
                    &run,
                    if pending { "verifying" } else { "review" },
                    &format!("result needs review: {error:#}"),
                )
            })?;
        }
    }
    Ok(())
}

pub fn cancel(store: &Store, rid: i64, task: &str) -> Result<Value> {
    store.atomic(|| {
        store.get_task(rid,task)?.with_context(||format!("unknown task {task}"))?;
        if let Some(r)=crate::reconcile::list(store,rid)?.into_iter().find(|r|r.task_id==task&&!matches!(r.state.as_str(),"completed"|"cancelled")) {
            crate::reconcile::cancel(store,rid,&r.id,"coordinator cancellation requested; retain shared scope until safe recovery")?;
        }
        for run in runs(store,rid)?.into_iter().filter(|run|run.task_id==task && (active(&run.state)||matches!(run.state.as_str(),"succeeded"|"verifying"))) {
            let path=Path::new(&run.run_dir);
            let state=if !path.exists() && matches!(run.state.as_str(),"preparing"|"starting") {
                // No launch can race this test: launch_starting takes this same
                // SQLite lock before atomically reserving the adapter directory.
                agent::cancel_unlaunched(path)?
            } else { agent::cancel(path)? };
            update(store,rid,&run,state.state(),"cancellation requested; reservations held until terminal receipt")?;
            if state.is_terminal() {
                store.close_assignment(rid,&run.assignment_id)?;
                update(store,rid,&run,"cancelled","cancelled; committed/uncommitted result preserved")?;
            }
        }
        store.set_task_state(rid,task,"cancelled")?;
        Ok(json!({"task_id":task,"runs":runs(store,rid)?.into_iter().filter(|run|run.task_id==task).collect::<Vec<_>>()}))
    })
}

pub fn dispatch(
    store: &Store,
    rid: i64,
    repo: &Path,
    cfg: &TowerConfig,
    action: &str,
    args: &Value,
) -> Result<Value> {
    let text = |key: &str| args[key].as_str().with_context(|| format!("missing {key}"));
    match action {
        "configure" => configure(
            store,
            rid,
            text("task_id")?,
            &serde_json::from_value(args["spec"].clone())?,
        ),
        "ready" => ready(store, rid, cfg),
        "status" => status(store, rid, cfg),
        "tick" => tick(store, rid, repo, cfg, text("holder")?),
        "cancel" => cancel(store, rid, text("task_id")?),
        _ => bail!("unknown scheduler action {action}"),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn dependency_cycles_and_failed_dependencies() {
        let s = Store::open_memory().unwrap();
        let r = s.register_repo("/fixture", "main").unwrap();
        let a = s.create_task(r.id, "a", "").unwrap();
        let b = s.create_task(r.id, "b", "").unwrap();
        let adapter = agent::AgentSpec {
            program: "fixture-agent".into(),
            args: vec![],
            timeout_secs: 30,
        };
        configure(
            &s,
            r.id,
            &a.id,
            &TaskSpec {
                depends_on: vec![b.id.clone()],
                agent: Some(adapter.clone()),
                ..Default::default()
            },
        )
        .unwrap();
        assert!(configure(
            &s,
            r.id,
            &b.id,
            &TaskSpec {
                depends_on: vec![a.id.clone()],
                ..Default::default()
            }
        )
        .is_err());
        s.set_task_state(r.id, &b.id, "failed").unwrap();
        assert_eq!(
            ready(&s, r.id, &TowerConfig::default()).unwrap()[0]["ready"],
            false
        );
        s.set_task_state(r.id, &b.id, "landed").unwrap();
        assert_eq!(
            ready(&s, r.id, &TowerConfig::default()).unwrap()[0]["ready"],
            true
        );
        let own = s
            .create_assignment(r.id, &a.id, "/fixture/worker", "worker")
            .unwrap();
        assert_eq!(
            ready(&s, r.id, &TowerConfig::default()).unwrap()[0]["ready"],
            false
        );
        s.close_assignment(r.id, &own.id).unwrap();
    }
    #[test]
    fn opt_in_budget_scope_and_immutable_running_spec() {
        let s = Store::open_memory().unwrap();
        let r = s.register_repo("/fixture", "main").unwrap();
        let a = s.create_task(r.id, "a", "").unwrap();
        assert!(ready(&s, r.id, &TowerConfig::default())
            .unwrap()
            .as_array()
            .unwrap()
            .is_empty());
        assert!(configure(
            &s,
            r.id,
            &a.id,
            &TaskSpec {
                max_attempts: 6,
                ..Default::default()
            }
        )
        .is_err());
        assert!(configure(
            &s,
            r.id,
            &a.id,
            &TaskSpec {
                nodes: vec![ClaimNode {
                    file: "../outside".into(),
                    item: None
                }],
                ..Default::default()
            }
        )
        .is_err());
        configure(&s, r.id, &a.id, &TaskSpec::default()).unwrap();
        s.set_task_state(r.id, &a.id, "running").unwrap();
        assert!(configure(&s, r.id, &a.id, &TaskSpec::default()).is_err());
    }

    struct Fixture {
        _temporary: tempfile::TempDir,
        repo: PathBuf,
        home: PathBuf,
        store: Store,
        rid: i64,
        cfg: TowerConfig,
    }

    fn fixture(script: &str) -> Fixture {
        let temporary = tempfile::tempdir().unwrap();
        let repo = temporary.path().join("repo");
        let home = temporary.path().join("home");
        std::fs::create_dir(&repo).unwrap();
        for args in [
            vec!["init", "-b", "main"],
            vec!["config", "user.email", "fixture@example.invalid"],
            vec!["config", "user.name", "Fixture"],
        ] {
            git::run_git(&repo, &args).unwrap();
        }
        std::fs::write(repo.join("shared.txt"), "base\n").unwrap();
        git::run_git(&repo, &["add", "."]).unwrap();
        git::run_git(&repo, &["commit", "-m", "fixture base"]).unwrap();
        let store = Store::open(home.clone()).unwrap();
        let rid = store
            .register_repo(&git::canonical_str(&repo), "main")
            .unwrap()
            .id;
        store
            .atomic(|| {
                store
                    .lease_acquire(rid, "fixture-owner")?
                    .map_err(anyhow::Error::msg)
            })
            .unwrap();
        let mut cfg = TowerConfig::default();
        cfg.scheduler.agent = Some(agent::AgentSpec {
            program: "/bin/sh".into(),
            args: vec!["-c".into(), script.into()],
            timeout_secs: 5,
        });
        Fixture {
            _temporary: temporary,
            repo,
            home,
            store,
            rid,
            cfg,
        }
    }

    fn scheduled(fixture: &Fixture, title: &str, attempts: u32) -> String {
        let task = fixture
            .store
            .create_task(fixture.rid, title, "fixture authored work")
            .unwrap();
        configure(
            &fixture.store,
            fixture.rid,
            &task.id,
            &TaskSpec {
                nodes: vec![ClaimNode {
                    file: "shared.txt".into(),
                    item: None,
                }],
                max_attempts: attempts,
                ..Default::default()
            },
        )
        .unwrap();
        task.id
    }

    #[test]
    fn failed_reconciler_does_not_requeue_a_new_owner_even_with_retry_budget() {
        let fx = fixture("exit 7");
        let mut participants = Vec::new();
        for name in ["left", "right"] {
            let path = fx.home.join(name);
            git::run_git(
                &fx.repo,
                &[
                    "worktree",
                    "add",
                    "-b",
                    name,
                    &path.to_string_lossy(),
                    "main",
                ],
            )
            .unwrap();
            let task = fx.store.create_task(fx.rid, name, "authored goal").unwrap();
            let a = fx
                .store
                .create_assignment(fx.rid, &task.id, &path.to_string_lossy(), name)
                .unwrap();
            participants.push(crate::reconcile::Intent {
                assignment_id: a.id,
                intent: name.into(),
                acceptance_tests: vec!["true".into()],
                ..Default::default()
            });
        }
        let r = crate::reconcile::request(
            &fx.store,
            fx.rid,
            participants,
            vec![ClaimNode {
                file: "shared.txt".into(),
                item: None,
            }],
        )
        .unwrap();
        for p in &r.participants {
            let a = fx
                .store
                .get_assignment(fx.rid, &p.assignment_id)
                .unwrap()
                .unwrap();
            let head = git::rev_parse(Path::new(&a.worktree_path), "HEAD").unwrap();
            crate::reconcile::pause(&fx.store, fx.rid, &r.id, &a.id, &head).unwrap();
        }
        configure(
            &fx.store,
            fx.rid,
            &r.task_id,
            &TaskSpec {
                max_attempts: 2,
                ..Default::default()
            },
        )
        .unwrap();
        let run = reserve_task(
            &fx.store,
            fx.rid,
            &fx.repo,
            &fx.cfg,
            "fixture-owner",
            &r.task_id,
        )
        .unwrap()
        .unwrap();
        prepare_worktrees(&fx.store, fx.rid, &fx.repo, &fx.cfg, "fixture-owner").unwrap();
        run_fixture_agent(&fx.store, fx.rid, &run);
        fx.store.atomic(|| observe_runs(&fx.store, fx.rid)).unwrap();
        assert_eq!(
            crate::reconcile::get(&fx.store, fx.rid, &r.id)
                .unwrap()
                .state,
            "cancelled_held"
        );
        assert_eq!(
            fx.store
                .get_task(fx.rid, &r.task_id)
                .unwrap()
                .unwrap()
                .state,
            "failed"
        );
        assert!(reserve_task(
            &fx.store,
            fx.rid,
            &fx.repo,
            &fx.cfg,
            "fixture-owner",
            &r.task_id
        )
        .unwrap()
        .is_none());
        assert_eq!(runs(&fx.store, fx.rid).unwrap().len(), 1);
        assert!(!fx
            .store
            .claims_for(fx.rid, &run.assignment_id)
            .unwrap()
            .is_empty());
    }

    // Execute only the fake configured fixture command. Avoid launch(), whose
    // current_exe() is a cargo test harness rather than the built Tower CLI.
    fn run_fixture_agent(store: &Store, rid: i64, run: &Run) {
        let raw: String = store
            .conn
            .query_row(
                "SELECT launch_json FROM agent_runs WHERE id=?1",
                [run.id],
                |row| row.get(0),
            )
            .unwrap();
        let launch: Launch = serde_json::from_str(&raw).unwrap();
        let assignment = store
            .get_assignment(rid, &run.assignment_id)
            .unwrap()
            .unwrap();
        let directory = Path::new(&run.run_dir);
        std::fs::create_dir_all(directory).unwrap();
        std::fs::write(directory.join("request.json"),serde_json::to_vec(&json!({"worktree":std::fs::canonicalize(&assignment.worktree_path).unwrap(),"spec":launch.agent})).unwrap()).unwrap();
        std::fs::write(directory.join("prompt.txt"), launch.prompt).unwrap();
        agent::run_supervisor(directory).unwrap();
    }

    #[test]
    fn reservation_rechecks_shared_scope_and_freezes_launch_before_git() {
        let fixture = fixture("exit 0");
        let first = scheduled(&fixture, "first", 1);
        let second = scheduled(&fixture, "second", 1);
        let run = reserve_task(
            &fixture.store,
            fixture.rid,
            &fixture.repo,
            &fixture.cfg,
            "fixture-owner",
            &first,
        )
        .unwrap()
        .unwrap();
        assert!(reserve_task(
            &fixture.store,
            fixture.rid,
            &fixture.repo,
            &fixture.cfg,
            "fixture-owner",
            &second
        )
        .unwrap()
        .is_none());
        assert_eq!(
            fixture.store.list_assignments(fixture.rid).unwrap().len(),
            1
        );
        assert_eq!(
            git::worktree_list(&fixture.repo).unwrap().len(),
            1,
            "reservation must commit before creating git worktree"
        );
        let raw: String = fixture
            .store
            .conn
            .query_row(
                "SELECT launch_json FROM agent_runs WHERE id=?1",
                [run.id],
                |row| row.get(0),
            )
            .unwrap();
        let frozen: Launch = serde_json::from_str(&raw).unwrap();
        assert_eq!(frozen.agent.args.last().unwrap(), "exit 0");
        assert!(frozen.prompt.contains("fixture authored work"));
        cancel(&fixture.store, fixture.rid, &first).unwrap();
        assert_eq!(
            agent::observe(Path::new(&run.run_dir)).unwrap(),
            agent::AgentStatus::Cancelled
        );
        assert!(fixture
            .store
            .claims_for(fixture.rid, &run.assignment_id)
            .unwrap()
            .is_empty());
        assert!(reserve_task(
            &fixture.store,
            fixture.rid,
            &fixture.repo,
            &fixture.cfg,
            "fixture-owner",
            &second
        )
        .unwrap()
        .is_some());
    }

    #[test]
    fn concurrent_same_holder_has_one_durable_reservation() {
        use std::sync::{Arc, Barrier};
        let fixture = fixture("exit 0");
        let task = scheduled(&fixture, "race", 1);
        let barrier = Arc::new(Barrier::new(2));
        let handles: Vec<_> = (0..2)
            .map(|_| {
                let home = fixture.home.clone();
                let repo = fixture.repo.clone();
                let cfg = fixture.cfg.clone();
                let task = task.clone();
                let barrier = barrier.clone();
                let rid = fixture.rid;
                std::thread::spawn(move || {
                    let store = Store::open(home).unwrap();
                    barrier.wait();
                    reserve_task(&store, rid, &repo, &cfg, "fixture-owner", &task)
                        .unwrap()
                        .is_some()
                })
            })
            .collect();
        assert_eq!(
            handles
                .into_iter()
                .map(|thread| thread.join().unwrap())
                .filter(|won| *won)
                .count(),
            1
        );
        assert_eq!(runs(&fixture.store, fixture.rid).unwrap().len(), 1);
        assert_eq!(
            fixture.store.list_assignments(fixture.rid).unwrap().len(),
            1
        );
    }

    #[test]
    fn failed_attempts_preserve_drafts_and_exhaust_bounded_budget() {
        let fixture = fixture("printf draft > shared.txt; exit 7");
        let task = scheduled(&fixture, "retry", 2);
        for attempt in 1..=2 {
            let run = reserve_task(
                &fixture.store,
                fixture.rid,
                &fixture.repo,
                &fixture.cfg,
                "fixture-owner",
                &task,
            )
            .unwrap()
            .unwrap();
            assert_eq!(run.attempt, attempt);
            prepare_worktrees(
                &fixture.store,
                fixture.rid,
                &fixture.repo,
                &fixture.cfg,
                "fixture-owner",
            )
            .unwrap();
            run_fixture_agent(&fixture.store, fixture.rid, &run);
            fixture
                .store
                .atomic(|| observe_runs(&fixture.store, fixture.rid))
                .unwrap();
            let assignment = fixture
                .store
                .get_assignment(fixture.rid, &run.assignment_id)
                .unwrap()
                .unwrap();
            assert_eq!(assignment.state, "closed");
            assert_eq!(
                std::fs::read_to_string(Path::new(&assignment.worktree_path).join("shared.txt"))
                    .unwrap(),
                "draft"
            );
            assert!(fixture
                .store
                .claims_for(fixture.rid, &assignment.id)
                .unwrap()
                .is_empty());
        }
        assert_eq!(
            fixture
                .store
                .get_task(fixture.rid, &task)
                .unwrap()
                .unwrap()
                .state,
            "failed"
        );
        assert!(reserve_task(
            &fixture.store,
            fixture.rid,
            &fixture.repo,
            &fixture.cfg,
            "fixture-owner",
            &task
        )
        .unwrap()
        .is_none());
        assert_eq!(
            git::worktree_list(&fixture.repo).unwrap().len(),
            3,
            "failed worktrees remain available for review"
        );
    }

    #[test]
    fn restarted_coordinator_observes_receipt_and_verifies_without_duplicate_changeset() {
        let fixture =
            fixture("printf completed > shared.txt; git add shared.txt; git commit -m completed");
        let task = scheduled(&fixture, "complete", 1);
        let run = reserve_task(
            &fixture.store,
            fixture.rid,
            &fixture.repo,
            &fixture.cfg,
            "fixture-owner",
            &task,
        )
        .unwrap()
        .unwrap();
        prepare_worktrees(
            &fixture.store,
            fixture.rid,
            &fixture.repo,
            &fixture.cfg,
            "fixture-owner",
        )
        .unwrap();
        run_fixture_agent(&fixture.store, fixture.rid, &run);
        let recovered = Store::open(fixture.home.clone()).unwrap();
        recovered
            .atomic(|| {
                recovered.lease_release(fixture.rid, "fixture-owner")?;
                recovered
                    .lease_acquire(fixture.rid, "replacement")?
                    .map_err(anyhow::Error::msg)?;
                observe_runs(&recovered, fixture.rid)
            })
            .unwrap();
        assert!(reserve_task(
            &fixture.store,
            fixture.rid,
            &fixture.repo,
            &fixture.cfg,
            "fixture-owner",
            &task
        )
        .is_err());
        verify_finished(
            &recovered,
            fixture.rid,
            &fixture.repo,
            &fixture.cfg,
            "replacement",
        )
        .unwrap();
        verify_finished(
            &recovered,
            fixture.rid,
            &fixture.repo,
            &fixture.cfg,
            "replacement",
        )
        .unwrap();
        assert_eq!(recovered.list_changesets(fixture.rid).unwrap().len(), 1);
        let final_run = runs(&recovered, fixture.rid).unwrap().pop().unwrap();
        assert_eq!(final_run.state, "review");
        assert!(final_run.changeset_id.is_some());
        assert_eq!(
            recovered
                .get_task(fixture.rid, &task)
                .unwrap()
                .unwrap()
                .state,
            "integrating"
        );
        assert_eq!(
            git::run_git(&fixture.repo, &["show", "main:shared.txt"]).unwrap(),
            "base\n",
            "verification must not integrate automatically"
        );
    }

    #[test]
    fn coordinator_drains_queued_gates_and_reuses_current_proof() {
        let mut fixture =
            fixture("printf completed > shared.txt; git add shared.txt; git commit -m completed");
        fixture.cfg.gates.check = Some("true".into());
        let task = scheduled(&fixture, "queued proof", 1);
        let run = reserve_task(
            &fixture.store,
            fixture.rid,
            &fixture.repo,
            &fixture.cfg,
            "fixture-owner",
            &task,
        )
        .unwrap()
        .unwrap();
        prepare_worktrees(
            &fixture.store,
            fixture.rid,
            &fixture.repo,
            &fixture.cfg,
            "fixture-owner",
        )
        .unwrap();
        run_fixture_agent(&fixture.store, fixture.rid, &run);
        fixture
            .store
            .atomic(|| observe_runs(&fixture.store, fixture.rid))
            .unwrap();
        let crate::resources::Reservation::Acquired(guard) = crate::resources::acquire(
            &fixture.store,
            fixture.rid,
            "fixture-other-gate",
            &crate::resources::resource_names(fixture.rid, &fixture.cfg),
            json!({"operation":"fixture"}),
        )
        .unwrap() else {
            panic!("fixture reservation")
        };
        verify_finished(
            &fixture.store,
            fixture.rid,
            &fixture.repo,
            &fixture.cfg,
            "fixture-owner",
        )
        .unwrap();
        let pending = runs(&fixture.store, fixture.rid).unwrap().pop().unwrap();
        assert_eq!(pending.state, "verifying");
        let cid = pending.changeset_id.unwrap();
        assert!(fixture
            .store
            .gate_runs_for(fixture.rid, &cid)
            .unwrap()
            .is_empty());
        drop(guard);
        drain_resources(
            &fixture.store,
            fixture.rid,
            &fixture.repo,
            &fixture.cfg,
            "fixture-owner",
        )
        .unwrap();
        verify_finished(
            &fixture.store,
            fixture.rid,
            &fixture.repo,
            &fixture.cfg,
            "fixture-owner",
        )
        .unwrap();
        assert_eq!(
            fixture
                .store
                .gate_runs_for(fixture.rid, &cid)
                .unwrap()
                .len(),
            1,
            "completed queue proof must not run a duplicate gate"
        );
        assert_eq!(
            runs(&fixture.store, fixture.rid)
                .unwrap()
                .pop()
                .unwrap()
                .state,
            "review"
        );
    }
}
