use anyhow::{bail, Context, Result};
use std::path::{Path, PathBuf};

use crate::config::{self, TowerConfig};
use crate::git;
use crate::store::Store;

pub fn worktree_create(store: &Store, repo_id: i64, repo: &Path, cfg: &TowerConfig, task_id: &str) -> Result<serde_json::Value> {
    let task = store.get_task(repo_id, task_id)?.with_context(|| format!("unknown task {task_id}"))?;
    let _ = task;
    // Peek next assignment number for path naming without consuming on failure: create assignment first, then worktree.
    let repo_str = git::canonical_str(repo);
    let base = store.home.join("worktrees").join(config::repo_hash(&repo_str));
    std::fs::create_dir_all(&base)?;
    // Temporary branch name requires assignment id; create assignment with placeholder then fix.
    let asg = store.create_assignment(repo_id, task_id, "", "")?;
    let branch = format!("tower/{}-{}", task_id.to_lowercase(), asg.id.to_lowercase());
    let dir: PathBuf = base.join(&asg.id);
    let mh = crate::engine::main_head(repo, cfg)?;
    let _ = mh;
    let (code, out, err) = git::run_git_raw(repo, &["worktree", "add", "-b", &branch, &dir.to_string_lossy(), &cfg.repo.main_branch]);
    if code != 0 {
        // Do not leave a phantom open assignment behind on failure.
        let _ = store.close_assignment(repo_id, &asg.id);
        bail!("worktree add failed: {} {}", out.trim(), err.trim());
    }
    store.conn.execute("UPDATE assignments SET worktree_path=?1, branch=?2 WHERE repo_id=?3 AND id=?4", rusqlite::params![dir.to_string_lossy(), branch, repo_id, asg.id])?;
    store.emit(repo_id, "worktree_created", serde_json::json!({"assignment_id": asg.id, "task_id": task_id, "worktree": dir.to_string_lossy(), "branch": branch}))?;
    store.set_task_state(repo_id, task_id, "running")?;
    Ok(serde_json::json!({"assignment_id": asg.id, "task_id": task_id, "worktree": dir.to_string_lossy(), "branch": branch}))
}
