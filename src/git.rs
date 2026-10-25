use anyhow::{bail, Context, Result};
use serde::{Deserialize, Serialize};
use std::path::{Path, PathBuf};
use std::process::Command;

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct WorktreeInfo {
    pub path: String,
    pub head: String,
    pub branch: Option<String>,
    pub bare: bool,
    pub detached: bool,
}

pub fn run_git(dir: &Path, args: &[&str]) -> Result<String> {
    let out = Command::new("git").current_dir(dir).args(args).output().with_context(|| format!("git {:?} in {}", args, dir.display()))?;
    if !out.status.success() {
        bail!("git {:?} failed: {}", args, String::from_utf8_lossy(&out.stderr).trim());
    }
    Ok(String::from_utf8_lossy(&out.stdout).into_owned())
}
pub fn run_git_raw(dir: &Path, args: &[&str]) -> (i32, String, String) {
    match Command::new("git").current_dir(dir).args(args).output() {
        Ok(o) => (o.status.code().unwrap_or(-1), String::from_utf8_lossy(&o.stdout).into_owned(), String::from_utf8_lossy(&o.stderr).into_owned()),
        Err(e) => (-1, String::new(), e.to_string()),
    }
}
pub fn repo_root(dir: &Path) -> Result<PathBuf> {
    let s = run_git(dir, &["rev-parse", "--show-toplevel"])?;
    Ok(PathBuf::from(s.trim()))
}
pub fn canonical_str(p: &Path) -> String {
    std::fs::canonicalize(p).map(|c| c.to_string_lossy().into_owned()).unwrap_or_else(|_| p.to_string_lossy().into_owned())
}
pub fn rev_parse(dir: &Path, rev: &str) -> Result<String> {
    Ok(run_git(dir, &["rev-parse", rev])?.trim().to_string())
}
pub fn merge_base(dir: &Path, a: &str, b: &str) -> Result<String> {
    Ok(run_git(dir, &["merge-base", a, b])?.trim().to_string())
}
pub fn current_branch(dir: &Path) -> Option<String> {
    run_git(dir, &["branch", "--show-current"]).ok().map(|s| s.trim().to_string()).filter(|s| !s.is_empty())
}
pub fn is_dirty(dir: &Path) -> bool {
    run_git(dir, &["status", "--porcelain"]).map(|s| !s.trim().is_empty()).unwrap_or(false)
}

pub fn worktree_list(repo: &Path) -> Result<Vec<WorktreeInfo>> {
    let out = run_git(repo, &["worktree", "list", "--porcelain"])?;
    let mut list = Vec::new();
    let mut cur: Option<WorktreeInfo> = None;
    for line in out.lines() {
        if let Some(rest) = line.strip_prefix("worktree ") {
            if let Some(c) = cur.take() { list.push(c); }
            cur = Some(WorktreeInfo { path: rest.trim().to_string(), head: String::new(), branch: None, bare: false, detached: false });
        } else if let Some(rest) = line.strip_prefix("HEAD ") {
            if let Some(c) = cur.as_mut() { c.head = rest.trim().to_string(); }
        } else if let Some(rest) = line.strip_prefix("branch ") {
            if let Some(c) = cur.as_mut() { c.branch = Some(rest.trim().strip_prefix("refs/heads/").unwrap_or(rest.trim()).to_string()); }
        } else if line == "bare" {
            if let Some(c) = cur.as_mut() { c.bare = true; }
        } else if line == "detached" {
            if let Some(c) = cur.as_mut() { c.detached = true; }
        }
    }
    if let Some(c) = cur.take() { list.push(c); }
    Ok(list)
}

pub fn changed_files(worktree: &Path, base: &str) -> Result<Vec<String>> {
    let mut set = std::collections::BTreeSet::new();
    let (code, out1, _) = run_git_raw(worktree, &["diff", "--name-only", base]);
    if code == 0 { for l in out1.lines() { if !l.trim().is_empty() { set.insert(l.trim().to_string()); } } }
    let (code2, out2, _) = run_git_raw(worktree, &["ls-files", "--others", "--exclude-standard"]);
    if code2 == 0 { for l in out2.lines() { if !l.trim().is_empty() { set.insert(l.trim().to_string()); } } }
    Ok(set.into_iter().collect())
}

pub fn file_at_ref(repo: &Path, rev: &str, path: &str) -> Option<String> {
    let spec = format!("{rev}:{path}");
    let (code, out, _) = run_git_raw(repo, &["show", &spec]);
    if code == 0 { Some(out) } else { None }
}

/// Build a commit capturing the working tree without touching user index/branch/worktree files.
/// Uses a temporary index file. Returns commit hash (HEAD if clean).
pub fn snapshot_commit(worktree: &Path) -> Result<String> {
    let head = rev_parse(worktree, "HEAD")?;
    if !is_dirty(worktree) { return Ok(head); }
    let tmp_idx = std::env::temp_dir().join(format!("tower-idx-{}-{}", std::process::id(), crate::store::now_secs()));
    let idx_s = tmp_idx.to_string_lossy().into_owned();
    let run = |args: &[&str]| -> Result<String> {
        // Fixed dates: the same tree+parent must snapshot to the same commit,
        // so gate proofs bound to a snapshot stay comparable (F6).
        let out = Command::new("git").current_dir(worktree).env("GIT_INDEX_FILE", &idx_s).env("GIT_AUTHOR_NAME", "Tower").env("GIT_AUTHOR_EMAIL", "tower@local").env("GIT_COMMITTER_NAME", "Tower").env("GIT_COMMITTER_EMAIL", "tower@local").env("GIT_AUTHOR_DATE", "2000-01-01T00:00:00Z").env("GIT_COMMITTER_DATE", "2000-01-01T00:00:00Z").args(args).output()?;
        if !out.status.success() { bail!("snapshot git {:?}: {}", args, String::from_utf8_lossy(&out.stderr).trim()); }
        Ok(String::from_utf8_lossy(&out.stdout).trim().to_string())
    };
    let _ = std::fs::remove_file(&tmp_idx);
    run(&["read-tree", &head])?;
    run(&["add", "-A"])?;
    let tree = run(&["write-tree"])?;
    let commit = run(&["commit-tree", &tree, "-p", &head, "-m", "tower snapshot"])?;
    let _ = std::fs::remove_file(&tmp_idx);
    Ok(commit)
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ConflictHunks { pub file: String, pub hunks: Vec<String> }

/// Conflicting hunk headers (`@@` lines) per file, from the classic
/// three-argument `git merge-tree <base> <a> <b>` output (its
/// "changed in both" sections carry the conflict diffs; the
/// `--write-tree` form used by `merge_sim` reports only file names).
/// Read-only. Only `only_files` are reported, in that order, so callers
/// keep `merge_sim` as the authority on which files conflict; a file
/// whose conflict carries no hunks (e.g. modify/delete) reports an
/// empty hunk list rather than disappearing.
pub fn merge_tree_conflict_hunks(repo: &Path, base: &str, a: &str, b: &str, only_files: &[String]) -> Vec<ConflictHunks> {
    let (_, out, _) = run_git_raw(repo, &["merge-tree", base, a, b]);
    let mut by_file: std::collections::HashMap<String, Vec<String>> = std::collections::HashMap::new();
    let mut current: Option<String> = None;
    for line in out.lines() {
        let trimmed = line.trim_start();
        let parts: Vec<&str> = trimmed.split_whitespace().collect();
        if parts.len() >= 4 && matches!(parts[0], "base" | "our" | "their") {
            current = Some(parts[3..].join(" "));
            continue;
        }
        if line.starts_with("@@") {
            if let Some(f) = &current {
                by_file.entry(f.clone()).or_default().push(line.trim_end().to_string());
            }
        }
    }
    only_files.iter().map(|f| ConflictHunks { file: f.clone(), hunks: by_file.remove(f).unwrap_or_default() }).collect()
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct SimResult { pub clean: bool, pub tree: Option<String>, pub conflict_files: Vec<String>, pub message: String }

pub fn merge_sim(repo: &Path, base: &str, a: &str, b: &str) -> SimResult {
    if a == b { return SimResult { clean: true, tree: None, conflict_files: vec![], message: "identical".into() }; }
    let mb = format!("--merge-base={base}");
    let out = Command::new("git").current_dir(repo).args(["merge-tree", "--write-tree", "--name-only", &mb, a, b]).output();
    match out {
        Ok(o) => {
            let stdout = String::from_utf8_lossy(&o.stdout).into_owned();
            let stderr = String::from_utf8_lossy(&o.stderr).into_owned();
            let code = o.status.code().unwrap_or(-1);
            let mut lines = stdout.lines();
            let tree = lines.next().map(|s| s.trim().to_string()).filter(|s| !s.is_empty());
            if code == 0 {
                SimResult { clean: true, tree, conflict_files: vec![], message: "clean".into() }
            } else {
                // With --name-only, after tree OID lines until blank are conflicted names.
                let mut files = Vec::new();
                for l in lines {
                    let t = l.trim();
                    if t.is_empty() { break; }
                    files.push(t.to_string());
                }
                SimResult { clean: false, tree, conflict_files: files, message: format!("conflict (exit {code}) {}", stderr.trim()) }
            }
        }
        Err(e) => SimResult { clean: false, tree: None, conflict_files: vec![], message: e.to_string() },
    }
}
