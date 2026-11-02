//! Cooperative semantic reconciliation. Every draft is pinned before ownership moves;
//! Tower never writes a participant's index, branch, or working files.
use anyhow::{bail, Context, Result};
use rusqlite::{params, OptionalExtension, TransactionBehavior};
use serde::{Deserialize, Serialize};
use serde_json::{json, Value};
use std::collections::HashMap;
use std::path::{Component, Path};

use crate::config::TowerConfig;
use crate::store::{now_secs, ClaimNode, Store};
use crate::{engine, git, index};

#[derive(Debug, Clone, Serialize, Deserialize, Default)]
pub struct Intent {
    pub assignment_id: String,
    pub intent: String,
    #[serde(default)]
    pub assumptions: Vec<String>,
    #[serde(default)]
    pub acceptance_tests: Vec<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Participant {
    pub assignment_id: String,
    pub intent: String,
    #[serde(default)]
    pub assumptions: Vec<String>,
    #[serde(default)]
    pub acceptance_tests: Vec<String>,
    pub draft_commit: String,
    pub paused_commit: Option<String>,
    pub frozen_scope: Vec<String>,
    pub stub_version: Option<u32>,
    pub resumed_head: Option<String>,
    pub validation_changeset: Option<String>,
    #[serde(default)]
    pub recovery_head: Option<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct TransferredClaim {
    pub assignment_id: String,
    pub node: ClaimNode,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Contract {
    pub version: u32,
    pub interface: String,
    /// Git object hash of the authored interface text (not a semantic assertion).
    pub hash: String,
    pub stub_commit: String,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Reconciliation {
    pub id: String,
    pub task_id: String,
    pub state: String,
    pub nodes: Vec<ClaimNode>,
    pub participants: Vec<Participant>,
    pub base_commit: String,
    pub contract: Option<Contract>,
    pub reconciler_assignment: Option<String>,
    pub result_commit: Option<String>,
    pub result_changeset: Option<String>,
    pub reason: Option<String>,
    pub revision: i64,
    #[serde(default)]
    pub transferred_claims: Vec<TransferredClaim>,
}

pub fn migrate(store: &Store) -> Result<()> {
    store.conn.execute_batch(
        "CREATE TABLE IF NOT EXISTS reconciliations(
            id INTEGER PRIMARY KEY AUTOINCREMENT,
            repo_id INTEGER NOT NULL,
            request_key TEXT NOT NULL,
            task_id TEXT NOT NULL DEFAULT '',
            state TEXT NOT NULL,
            data_json TEXT NOT NULL,
            revision INTEGER NOT NULL DEFAULT 0,
            created_at INTEGER NOT NULL,
            UNIQUE(repo_id,request_key));
         CREATE TABLE IF NOT EXISTS reconciliation_checks(
            id INTEGER PRIMARY KEY AUTOINCREMENT,
            repo_id INTEGER NOT NULL,
            reconciliation_id TEXT NOT NULL,
            data_json TEXT NOT NULL);",
    )?;
    Ok(())
}

fn transaction(store: &Store) -> Result<Option<rusqlite::Transaction<'_>>> {
    if store.conn.is_autocommit() {
        Ok(Some(rusqlite::Transaction::new_unchecked(
            &store.conn,
            TransactionBehavior::Immediate,
        )?))
    } else {
        Ok(None)
    }
}
fn commit_transaction(tx: Option<rusqlite::Transaction<'_>>) -> Result<()> {
    if let Some(tx) = tx {
        tx.commit()?;
    }
    Ok(())
}

fn number(id: &str) -> Result<i64> {
    let n = id
        .strip_prefix("R-")
        .context("expected reconciliation ID R-<number>")?
        .parse::<i64>()?;
    if n < 1 {
        bail!("invalid reconciliation ID");
    }
    Ok(n)
}

pub fn list(store: &Store, repo_id: i64) -> Result<Vec<Reconciliation>> {
    migrate(store)?;
    let mut q = store
        .conn
        .prepare("SELECT data_json FROM reconciliations WHERE repo_id=?1 ORDER BY id")?;
    let rows = q.query_map(params![repo_id], |r| r.get::<_, String>(0))?;
    rows.map(|r| Ok(serde_json::from_str(&r?)?)).collect()
}

pub fn get(store: &Store, repo_id: i64, id: &str) -> Result<Reconciliation> {
    migrate(store)?;
    let data: Option<String> = store
        .conn
        .query_row(
            "SELECT data_json FROM reconciliations WHERE repo_id=?1 AND id=?2",
            params![repo_id, number(id)?],
            |r| r.get(0),
        )
        .optional()?;
    serde_json::from_str(&data.with_context(|| format!("unknown reconciliation {id}"))?)
        .map_err(Into::into)
}

fn save(store: &Store, repo_id: i64, r: &mut Reconciliation) -> Result<()> {
    let revision = r.revision;
    r.revision += 1;
    let changed = store.conn.execute(
        "UPDATE reconciliations SET state=?1,task_id=?2,data_json=?3,revision=?4 WHERE repo_id=?5 AND id=?6 AND revision=?7",
        params![r.state,r.task_id,serde_json::to_string(r)?,r.revision,repo_id,number(&r.id)?,revision],
    )?;
    if changed != 1 {
        bail!("reconciliation changed concurrently; reload {}", r.id);
    }
    store.emit(
        repo_id,
        "reconciliation_updated",
        json!({"reconciliation_id":r.id,"state":r.state,"revision":r.revision}),
    )?;
    Ok(())
}

fn active(r: &Reconciliation) -> bool {
    !matches!(r.state.as_str(), "completed" | "cancelled")
}

fn overlaps(a: &ClaimNode, b: &ClaimNode) -> bool {
    a.file == b.file && (a.item.is_none() || b.item.is_none() || a.item == b.item)
}

fn valid_nodes(nodes: &[ClaimNode]) -> Result<()> {
    if nodes.is_empty() {
        bail!("reconciliation requires at least one shared node");
    }
    for n in nodes {
        if n.file.is_empty()
            || Path::new(&n.file)
                .components()
                .any(|c| !matches!(c, Component::Normal(_)))
        {
            bail!("shared node must be a repository-relative file");
        }
        if n.item.as_ref().is_some_and(|s| s.trim().is_empty()) {
            bail!("empty item name");
        }
    }
    Ok(())
}

fn assignment_path(store: &Store, repo_id: i64, id: &str) -> Result<std::path::PathBuf> {
    let a = store
        .get_assignment(repo_id, id)?
        .with_context(|| format!("unknown assignment {id}"))?;
    if a.state != "open" || a.worktree_path.is_empty() {
        bail!("assignment {id} must be open with a worktree");
    }
    let path = std::path::PathBuf::from(&a.worktree_path);
    if !path.exists() {
        bail!("assignment {id} worktree is missing");
    }
    // Do not accept an unrelated repository substituted into an assignment.
    let expected: String = store.conn.query_row(
        "SELECT path FROM repos WHERE id=?1",
        params![repo_id],
        |r| r.get(0),
    )?;
    let actual_common = git::run_git(
        &path,
        &["rev-parse", "--path-format=absolute", "--git-common-dir"],
    )?;
    let expected_common = git::run_git(
        Path::new(&expected),
        &["rev-parse", "--path-format=absolute", "--git-common-dir"],
    )?;
    if actual_common.trim() != expected_common.trim() {
        bail!("assignment {id} belongs to a different git repository");
    }
    Ok(path)
}

fn commit(repo: &Path, head: &str) -> Result<String> {
    if head.starts_with('-') {
        bail!("expected a commit object ID");
    }
    let oid = git::rev_parse(repo, &format!("{head}^{{commit}}"))?;
    if oid.len() != 40 && oid.len() != 64 {
        bail!("invalid commit object ID");
    }
    Ok(oid)
}

fn clean_head(path: &Path, expected: &str) -> Result<String> {
    let status = git::run_git(path, &["status", "--porcelain"])?;
    if !status.trim().is_empty() {
        bail!("handoff acknowledgement requires a clean committed worktree");
    }
    let head = git::rev_parse(path, "HEAD")?;
    if head != commit(path, expected)? {
        bail!("worktree HEAD changed; acknowledge its exact current commit");
    }
    Ok(head)
}

fn ancestor(repo: &Path, older: &str, newer: &str) -> bool {
    git::run_git_raw(repo, &["merge-base", "--is-ancestor", older, newer]).0 == 0
}

fn pin(repo: &Path, id: &str, label: &str, oid: &str) -> Result<()> {
    number(id)?;
    // Labels are generated internally, never read from a participant.
    git::run_git(
        repo,
        &[
            "update-ref",
            &format!("refs/tower/reconciliations/{id}/{label}"),
            oid,
        ],
    )?;
    Ok(())
}

fn hash_text(repo: &Path, text: &str) -> Result<String> {
    use std::io::Write;
    let mut child = std::process::Command::new("git")
        .current_dir(repo)
        .args(["hash-object", "--stdin"])
        .stdin(std::process::Stdio::piped())
        .stdout(std::process::Stdio::piped())
        .spawn()?;
    child
        .stdin
        .take()
        .context("hash stdin")?
        .write_all(text.as_bytes())?;
    let output = child.wait_with_output()?;
    if !output.status.success() {
        bail!("cannot hash shared source");
    }
    Ok(String::from_utf8(output.stdout)?.trim().into())
}

/// Git hashes of exact fragments; ambiguous/missing names freeze the whole file.
/// Store hashes rather than copying private source into board metadata.
fn fingerprints(repo: &Path, head: &str, nodes: &[ClaimNode]) -> Result<Vec<String>> {
    nodes
        .iter()
        .map(|n| {
            let Some(source) = git::file_at_ref(repo, head, &n.file) else {
                return Ok("<deleted>".into());
            };
            let Some(name) = &n.item else {
                return hash_text(repo, &source);
            };
            let matched: Vec<_> = index::extract_items(&n.file, &source)
                .into_iter()
                .filter(|i| &i.name == name)
                .collect();
            if matched.len() != 1 {
                return hash_text(repo, &source);
            }
            let i = &matched[0];
            hash_text(
                repo,
                &source
                    .lines()
                    .skip(i.start_line.saturating_sub(1))
                    .take(i.end_line - i.start_line + 1)
                    .collect::<Vec<_>>()
                    .join("\n"),
            )
        })
        .collect()
}

fn refresh_ready(r: &mut Reconciliation) {
    let ready = r.participants.iter().all(|p| {
        !p.acceptance_tests.is_empty()
            && p.paused_commit.is_some()
            && r.contract
                .as_ref()
                .is_none_or(|c| p.stub_version == Some(c.version))
    });
    r.state = if ready { "ready" } else { "awaiting_handoffs" }.into();
}

/// Explicit authored needs. Duplicate requests for the same snapshots, nodes, and
/// intents return the existing record even after completion; changes need new input.
pub fn request(
    store: &Store,
    repo_id: i64,
    mut intents: Vec<Intent>,
    mut nodes: Vec<ClaimNode>,
) -> Result<Reconciliation> {
    migrate(store)?;
    valid_nodes(&nodes)?;
    if intents.len() != 2
        || intents[0].assignment_id == intents[1].assignment_id
        || intents.iter().any(|i| i.intent.trim().is_empty())
    {
        bail!("provide exactly two distinct assignments and a nonempty authored intent for each");
    }
    intents.sort_by(|a, b| a.assignment_id.cmp(&b.assignment_id));
    // A broad claim cannot be split safely without a complete language index.
    // Widen that overlap to a file instead of silently discarding nonoverlap claims.
    let claims = store.list_claims(repo_id)?;
    for n in &mut nodes {
        if claims.iter().any(|c| {
            intents.iter().any(|i| i.assignment_id == c.assignment_id)
                && c.file_path == n.file
                && c.item_name.is_none()
        }) {
            n.item = None;
        }
    }
    nodes.sort_by(|a, b| (&a.file, &a.item).cmp(&(&b.file, &b.item)));
    nodes.dedup();
    let mut participants = Vec::new();
    for i in intents {
        let path = assignment_path(store, repo_id, &i.assignment_id)?;
        if i.acceptance_tests.len() > 32
            || i.acceptance_tests
                .iter()
                .any(|s| s.trim().is_empty() || s.len() > 8192)
        {
            bail!("acceptance_tests must contain at most 32 nonempty commands, each at most 8192 bytes");
        }
        participants.push(Participant {
            assignment_id: i.assignment_id,
            intent: i.intent,
            assumptions: i.assumptions,
            acceptance_tests: i.acceptance_tests,
            draft_commit: git::snapshot_commit(&path)?,
            paused_commit: None,
            frozen_scope: vec![],
            stub_version: None,
            resumed_head: None,
            validation_changeset: None,
            recovery_head: None,
        });
    }
    let key = serde_json::to_string(
        &json!({"participants":participants.iter().map(|p|json!([p.assignment_id,p.intent,p.assumptions,p.acceptance_tests,p.draft_commit])).collect::<Vec<_>>(),"nodes":nodes}),
    )?;
    let tx = transaction(store)?;
    let existing: Option<String> = store
        .conn
        .query_row(
            "SELECT data_json FROM reconciliations WHERE repo_id=?1 AND request_key=?2",
            params![repo_id, key],
            |r| r.get(0),
        )
        .optional()?;
    if let Some(data) = existing {
        commit_transaction(tx)?;
        return Ok(serde_json::from_str(&data)?);
    }
    for existing in list(store, repo_id)?.into_iter().filter(active) {
        if existing
            .nodes
            .iter()
            .any(|a| nodes.iter().any(|b| overlaps(a, b)))
        {
            bail!("shared scope already reserved by {}; add requirements to the existing handoff instead of starting another reconciler",existing.id);
        }
    }
    let repo = assignment_path(store, repo_id, &participants[0].assignment_id)?;
    let base = git::merge_base(
        &repo,
        &participants[0].draft_commit,
        &participants[1].draft_commit,
    )?;
    store.conn.execute("INSERT INTO reconciliations(repo_id,request_key,state,data_json,created_at) VALUES(?1,?2,'awaiting_handoffs','{}',?3)",params![repo_id,key,now_secs()])?;
    let id = format!("R-{}", store.conn.last_insert_rowid());
    let brief=format!("Reconcile shared scope for {id}. Wait for both cooperative handoffs. Tower will supply both authored intents, immutable draft commits and any provisional contract when dispatch becomes ready.");
    let task = store.create_task(repo_id, &format!("Reconcile shared edits {id}"), &brief)?;
    crate::scheduler::configure(
        store,
        repo_id,
        &task.id,
        &crate::scheduler::TaskSpec::default(),
    )?;
    let mut r = Reconciliation {
        id,
        task_id: task.id,
        state: "awaiting_handoffs".into(),
        nodes,
        participants,
        base_commit: base,
        contract: None,
        reconciler_assignment: None,
        result_commit: None,
        result_changeset: None,
        reason: None,
        revision: 0,
        transferred_claims: vec![],
    };
    for (n, p) in r.participants.iter().enumerate() {
        pin(&repo, &r.id, &format!("draft-{n}"), &p.draft_commit)?;
    }
    save(store, repo_id, &mut r)?;
    commit_transaction(tx)?;
    Ok(r)
}

pub fn update_intent(
    store: &Store,
    repo_id: i64,
    id: &str,
    value: Intent,
) -> Result<Reconciliation> {
    if value.intent.trim().is_empty()
        || value.acceptance_tests.len() > 32
        || value
            .acceptance_tests
            .iter()
            .any(|s| s.trim().is_empty() || s.len() > 8192)
    {
        bail!("provide an authored goal and at most 32 nonempty acceptance commands");
    }
    let tx = transaction(store)?;
    let mut r = get(store, repo_id, id)?;
    if !matches!(r.state.as_str(), "awaiting_handoffs" | "ready") {
        bail!("requirements cannot change after dispatch; cancel and renegotiate");
    }
    let p = r
        .participants
        .iter_mut()
        .find(|p| p.assignment_id == value.assignment_id)
        .context("assignment is not a participant")?;
    p.intent = value.intent;
    p.assumptions = value.assumptions;
    p.acceptance_tests = value.acceptance_tests;
    p.paused_commit = None;
    p.frozen_scope.clear();
    let invalidated = r
        .participants
        .iter()
        .filter(|p| p.stub_version.is_some())
        .count();
    for p in &mut r.participants {
        p.stub_version = None;
    }
    store.emit(repo_id,"reconciliation_intent_updated",json!({"reconciliation_id":r.id,"assignment_id":value.assignment_id,"invalidated_stub_acknowledgements":invalidated}))?;
    refresh_ready(&mut r);
    save(store, repo_id, &mut r)?;
    commit_transaction(tx)?;
    Ok(r)
}

pub fn pause(
    store: &Store,
    repo_id: i64,
    id: &str,
    assignment: &str,
    head: &str,
) -> Result<Reconciliation> {
    let tx = transaction(store)?;
    let mut r = get(store, repo_id, id)?;
    if !matches!(r.state.as_str(), "awaiting_handoffs" | "ready") {
        bail!("handoff cannot change after reconciliation dispatch");
    }
    let path = assignment_path(store, repo_id, assignment)?;
    let head = clean_head(&path, head)?;
    let idx = r
        .participants
        .iter()
        .position(|p| p.assignment_id == assignment)
        .context("assignment is not a participant")?;
    let old = &r.participants[idx].draft_commit;
    // A dirty snapshot is a synthetic child of the original HEAD. Its tree must
    // survive in the committed handoff, or it must itself be an ancestor.
    let old_tree = git::rev_parse(&path, &format!("{old}^{{tree}}"))?;
    if old_tree != git::rev_parse(&path, "HEAD^{tree}")? && !ancestor(&path, old, &head) {
        // Dirty snapshots use a synthetic child commit. A worker may commit the
        // same shared draft while advancing independent work, producing a
        // sibling commit rather than that synthetic commit's descendant.
        let synthetic =
            git::run_git(&path, &["show", "-s", "--format=%s", old])?.trim() == "tower snapshot";
        let parent = git::rev_parse(&path, &format!("{old}^"));
        if !synthetic
            || !parent.is_ok_and(|parent| ancestor(&path, &parent, &head))
            || fingerprints(&path, old, &r.nodes)? != fingerprints(&path, &head, &r.nodes)?
        {
            bail!("handoff must preserve the captured shared draft and original commit ancestry");
        }
    }
    r.participants[idx].paused_commit = Some(head.clone());
    r.participants[idx].frozen_scope = fingerprints(&path, &head, &r.nodes)?;
    r.participants[idx].stub_version = None;
    pin(&path, &r.id, &format!("paused-{idx}"), &head)?;
    refresh_ready(&mut r);
    save(store, repo_id, &mut r)?;
    commit_transaction(tx)?;
    Ok(r)
}

pub fn contract_set(
    store: &Store,
    repo_id: i64,
    id: &str,
    interface: &str,
    stub: &str,
) -> Result<Reconciliation> {
    if interface.trim().is_empty() {
        bail!("provide the explicitly authored common interface; Tower does not invent it");
    }
    let tx = transaction(store)?;
    let mut r = get(store, repo_id, id)?;
    if !matches!(r.state.as_str(), "awaiting_handoffs" | "ready") {
        bail!("contract cannot change after dispatch; cancel and negotiate a new reconciliation");
    }
    let repo = assignment_path(store, repo_id, &r.participants[0].assignment_id)?;
    let stub = commit(&repo, stub)?;
    if !ancestor(&repo, &r.base_commit, &stub) {
        bail!("stub must descend the shared base");
    }
    ensure_commit_scope(&repo, &r.base_commit, &stub, &r.nodes)?;
    let hash = hash_text(&repo, interface)?;
    let version = r.contract.as_ref().map_or(1, |c| c.version + 1);
    let invalidated = r
        .participants
        .iter()
        .filter(|p| p.stub_version.is_some())
        .count();
    r.contract = Some(Contract {
        version,
        interface: interface.into(),
        hash,
        stub_commit: stub.clone(),
    });
    for p in &mut r.participants {
        p.stub_version = None;
    }
    store.emit(repo_id,"reconciliation_contract_updated",json!({"reconciliation_id":r.id,"version":version,"invalidated_stub_acknowledgements":invalidated}))?;
    pin(&repo, &r.id, &format!("stub-{version}"), &stub)?;
    refresh_ready(&mut r);
    save(store, repo_id, &mut r)?;
    commit_transaction(tx)?;
    Ok(r)
}

pub fn stub_ack(
    store: &Store,
    repo_id: i64,
    id: &str,
    assignment: &str,
    head: &str,
    version: u32,
) -> Result<Reconciliation> {
    let tx = transaction(store)?;
    let mut r = get(store, repo_id, id)?;
    if !matches!(r.state.as_str(), "awaiting_handoffs" | "ready") {
        bail!("stub acknowledgement is closed after dispatch");
    }
    let c = r.contract.as_ref().context("no provisional contract")?;
    if c.version != version {
        bail!("contract changed; acknowledge the current version");
    }
    let path = assignment_path(store, repo_id, assignment)?;
    let head = clean_head(&path, head)?;
    let p = r
        .participants
        .iter_mut()
        .find(|p| p.assignment_id == assignment)
        .context("assignment is not a participant")?;
    let paused = p
        .paused_commit
        .as_ref()
        .context("pause and pin the original draft before installing a stub")?;
    if !ancestor(&path, paused, &head) || !ancestor(&path, &c.stub_commit, &head) {
        bail!("stub acknowledgement must contain both the paused draft and exact stub commit");
    }
    if fingerprints(&path, &head, &r.nodes)? != fingerprints(&path, &c.stub_commit, &r.nodes)? {
        bail!("stub acknowledgement must install the exact shared provisional implementation, not retain a different merge resolution");
    }
    p.stub_version = Some(version);
    p.frozen_scope = fingerprints(&path, &head, &r.nodes)?;
    refresh_ready(&mut r);
    save(store, repo_id, &mut r)?;
    commit_transaction(tx)?;
    Ok(r)
}

fn ensure_frozen(store: &Store, repo_id: i64, r: &Reconciliation) -> Result<()> {
    for p in &r.participants {
        let path = assignment_path(store, repo_id, &p.assignment_id)?;
        let current = git::snapshot_commit(&path)?;
        let paused = p
            .paused_commit
            .as_ref()
            .context("participant has not paused shared edits")?;
        if !ancestor(&path, paused, &current)
            || fingerprints(&path, &current, &r.nodes)? != p.frozen_scope
        {
            bail!("{} changed the frozen shared scope; reconciliation must be cancelled and renegotiated",p.assignment_id);
        }
    }
    Ok(())
}

pub fn scheduler_task_ready(store: &Store, repo_id: i64, task_id: &str) -> Result<bool> {
    Ok(list(store, repo_id)?
        .into_iter()
        .find(|r| r.task_id == task_id)
        .is_none_or(|r| r.state == "ready" || r.state == "dispatched"))
}

pub fn prepare_dispatch(
    store: &Store,
    repo_id: i64,
    task_id: &str,
    assignment: &str,
) -> Result<Option<String>> {
    let Some(mut r) = list(store, repo_id)?
        .into_iter()
        .find(|r| r.task_id == task_id)
    else {
        return Ok(None);
    };
    let tx = transaction(store)?;
    // Read again under the write lock, so two coordinators cannot transfer twice.
    r = get(store, repo_id, &r.id)?;
    if r.state == "dispatched" {
        if r.reconciler_assignment.as_deref() != Some(assignment) {
            bail!("reconciliation already belongs to another assignment");
        }
        commit_transaction(tx)?;
        return Ok(Some(brief(&r)));
    }
    if r.state != "ready" {
        bail!("reconciliation is waiting for cooperative handoffs");
    }
    ensure_frozen(store, repo_id, &r)?;
    let worker = store
        .get_assignment(repo_id, assignment)?
        .context("unknown reconciler assignment")?;
    if worker.task_id != r.task_id || worker.state != "open" {
        bail!("reconciler must be an open assignment for the reconciliation task");
    }
    for c in store.list_claims(repo_id)? {
        if !r.nodes.iter().any(|n| {
            overlaps(
                n,
                &ClaimNode {
                    file: c.file_path.clone(),
                    item: c.item_name.clone(),
                },
            )
        }) {
            continue;
        }
        if !r
            .participants
            .iter()
            .any(|p| p.assignment_id == c.assignment_id)
            && c.assignment_id != assignment
        {
            bail!(
                "shared scope is held by a third assignment {}",
                c.assignment_id
            );
        }
        if c.assignment_id != assignment {
            r.transferred_claims.push(TransferredClaim {
                assignment_id: c.assignment_id.clone(),
                node: ClaimNode {
                    file: c.file_path.clone(),
                    item: c.item_name.clone(),
                },
            });
        }
        store.conn.execute(
            "DELETE FROM claims WHERE repo_id=?1 AND id=?2",
            params![repo_id, c.id],
        )?;
    }
    for n in &r.nodes {
        store.conn.execute("INSERT INTO claims(repo_id,assignment_id,file_path,item_name,created_at) VALUES(?1,?2,?3,?4,?5)",params![repo_id,assignment,n.file,n.item,now_secs()])?;
    }
    r.reconciler_assignment = Some(assignment.into());
    r.state = "dispatched".into();
    save(store, repo_id, &mut r)?;
    commit_transaction(tx)?;
    Ok(Some(brief(&r)))
}

fn brief(r: &Reconciliation) -> String {
    format!("Reconciliation {}. Implement the shared change satisfying BOTH authored intents.\nShared nodes: {}\nFrozen common base: {}\nInputs (immutable commits pinned by Tower):\n{}\n{}\nRead each input's shared diff and preserve its requirements. Implement only shared nodes in this worktree. Do not change original worktrees or claim independent scope. Do not pretend a text merge proves intent. Commit the combined implementation and tests. Preserve the common base{} as ancestors. Configured gates must pass before Tower returns the commit. Original workers will merge the returned commit and validate their downstream work.\n",r.id,serde_json::to_string(&r.nodes).unwrap_or_default(),r.base_commit,r.participants.iter().map(|p|format!("{} goal: {}\nInterface assumptions: {}\nExplicit acceptance commands: {}\nrequest snapshot: {}\npaused committed draft: {}",p.assignment_id,p.intent,serde_json::to_string(&p.assumptions).unwrap_or_default(),serde_json::to_string(&p.acceptance_tests).unwrap_or_default(),p.draft_commit,p.paused_commit.as_deref().unwrap_or("pending"))).collect::<Vec<_>>().join("\n"),r.contract.as_ref().map(|c|format!("PROVISIONAL CONTRACT v{} hash {}\n{}\nStub commit {} is unresolved; replace stub behavior, retain the authored interface.",c.version,c.hash,c.interface,c.stub_commit)).unwrap_or_else(||"No stable provisional contract: original agents must keep overlap-dependent work paused; independent work may continue.".into()),r.contract.as_ref().map(|c|format!(" and stub {}",c.stub_commit)).unwrap_or_default())
}

fn ensure_commit_scope(repo: &Path, base: &str, head: &str, nodes: &[ClaimNode]) -> Result<()> {
    let paths = git::run_git(repo, &["diff", "--name-only", base, head, "--"])?;
    let mut files = Vec::new();
    let mut changed = Vec::new();
    for file in paths.lines().filter(|f| !f.is_empty()) {
        files.push(file.into());
        let before = git::file_at_ref(repo, base, file);
        let after = git::file_at_ref(repo, head, file);
        let allowed: Vec<_> = nodes.iter().filter(|n| n.file == file).collect();
        if !allowed.iter().any(|n| n.item.is_none()) {
            // Item indexing cannot cover imports, attributes, whitespace around
            // declarations, or two declarations sharing one line reliably. Keep
            // all such text byte-for-byte unchanged, otherwise widen the handoff.
            let mask = |source: Option<&str>| -> Result<Vec<String>> {
                let Some(source) = source else {
                    return Ok(vec![]);
                };
                let indexed = index::extract_items(file, source);
                let mut covered = vec![false; source.lines().count()];
                for n in &allowed {
                    let found: Vec<_> = indexed
                        .iter()
                        .filter(|i| Some(&i.name) == n.item.as_ref())
                        .collect();
                    if found.len() > 1 {
                        bail!(
                            "ambiguous item {} in {file}; negotiate a whole-file handoff",
                            n.item.as_deref().unwrap_or("")
                        );
                    }
                    if let Some(i) = found.first() {
                        for other in &indexed {
                            if other.name != i.name
                                && !matches!(other.kind.as_str(), "impl" | "class")
                                && other.start_line <= i.end_line
                                && i.start_line <= other.end_line
                            {
                                bail!(
                                    "item spans overlap in {file}; negotiate a whole-file handoff"
                                );
                            }
                        }
                        for line in i.start_line.saturating_sub(1)..i.end_line.min(covered.len()) {
                            covered[line] = true;
                        }
                    }
                }
                Ok(source
                    .lines()
                    .enumerate()
                    .filter(|(line, _)| !covered[*line])
                    .map(|(_, line)| line.into())
                    .collect())
            };
            if mask(before.as_deref())? != mask(after.as_deref())? {
                bail!("unindexed text changed outside shared items in {file}; negotiate a whole-file handoff");
            }
        }
        let items = index::changed_items(file, before.as_deref(), after.as_deref());
        if items.is_empty() && !nodes.iter().any(|n| n.file == file && n.item.is_none()) {
            bail!("cannot prove item scope for {file}; negotiate a whole-file handoff");
        }
        for item in items {
            changed.push((file.into(), item));
        }
    }
    let cva = engine::claimed_vs_actual(nodes, &files, &changed);
    if engine::outside_claim(&cva) {
        bail!(
            "result edits outside shared scope: {}",
            engine::outside_reason(&cva)
        );
    }
    Ok(())
}

/// Reconciliation requires current proof fields, including on pre-0.3 homes.
fn proven_changeset(
    store: &Store,
    repo_id: i64,
    repo: &Path,
    cfg: &TowerConfig,
    id: &str,
) -> Result<crate::store::ChangesetRow> {
    if !cfg.has_gates() {
        bail!("reconciliation requires at least one configured validation gate");
    }
    engine::refresh_changeset_statuses(store, repo_id, repo, cfg)?;
    let cs = store
        .get_changeset(repo_id, id)?
        .with_context(|| format!("unknown changeset {id}"))?;
    if cs.status != "green" || cs.superseded {
        bail!(
            "reconciliation requires a current green changeset; {id} is {}: {}",
            cs.status,
            cs.reasons_json
        );
    }
    let path = assignment_path(store, repo_id, &cs.assignment_id)?;
    let head = clean_head(&path, &cs.head_commit)?;
    let snapshots: HashMap<_, _> = store
        .list_changesets(repo_id)?
        .into_iter()
        .map(|c| {
            let snap = git::snapshot_commit(Path::new(&c.worktree_path)).unwrap_or(c.head_commit);
            (c.id, snap)
        })
        .collect();
    let main = engine::main_head(repo, cfg)?;
    let gates = store.gate_runs_for(repo_id, id)?;
    for (tier, configured) in [
        (1, cfg.gates.check.is_some()),
        (2, cfg.gates.test.is_some()),
    ] {
        if !configured {
            continue;
        }
        let g = gates
            .iter()
            .rev()
            .find(|g| g.tier == tier)
            .context("configured validation gate has not run")?;
        let participant: Value = serde_json::from_str(
            g.participants_json
                .as_deref()
                .context("legacy gate has no participant proof; rerun validation")?,
        )?;
        if !g.passed
            || g.tree_hash.is_none()
            || g.main_head.is_none()
            || participant.get(id).and_then(Value::as_str) != Some(head.as_str())
            || !engine::gate_proof_current(g, &snapshots, &main)
        {
            bail!("validation receipt is failed or stale; rerun configured gates");
        }
    }
    Ok(cs)
}

fn acceptance(
    store: &Store,
    repo_id: i64,
    cfg: &TowerConfig,
    r: &Reconciliation,
    stage: &str,
    assignment: &str,
    head: &str,
) -> Result<()> {
    if !store.conn.is_autocommit() {
        bail!("acceptance validation must run outside a database transaction");
    }
    if r.participants.iter().any(|p| p.acceptance_tests.is_empty()) {
        bail!("both participants must author acceptance_tests before reconciliation can return");
    }
    let path = assignment_path(store, repo_id, assignment)?;
    clean_head(&path, head)?;
    let tree = git::rev_parse(&path, "HEAD^{tree}")?;
    let commands: Vec<_> = r
        .participants
        .iter()
        .filter(|p| stage == "combined" || p.assignment_id == assignment)
        .flat_map(|p| {
            p.acceptance_tests
                .iter()
                .map(move |cmd| (p.assignment_id.clone(), cmd.clone()))
        })
        .collect();
    let key = format!(
        "reconcile:{}:{stage}:{assignment}:{head}:{}",
        r.id,
        r.contract.as_ref().map_or(0, |c| c.version)
    );
    let mut reservation=match crate::resources::acquire(store,repo_id,&key,&crate::resources::resource_names(repo_id,cfg),json!({"operation":"reconcile_acceptance","reconciliation_id":r.id,"stage":stage,"assignment_id":assignment,"head":head}))? {
        crate::resources::Reservation::Acquired(guard)=>guard,
        crate::resources::Reservation::Queued(job)=>bail!("acceptance validation queued: resource job {}; retry the same reconciliation operation",job.id),
    };
    for (owner, command) in commands {
        clean_head(&path, head)?;
        let started = std::time::Instant::now();
        let (passed, _output) = reservation.run_gate(&path, &command, cfg.gates.timeout_secs)?;
        let unchanged = clean_head(&path, head).is_ok();
        let receipt = json!({"stage":stage,"requirements_owner":owner,"validated_assignment":assignment,"head":head,"tree_hash":tree,"contract_version":r.contract.as_ref().map(|c|c.version),"command":command,"passed":passed&&unchanged,"duration_ms":started.elapsed().as_millis() as u64,"created_at":now_secs(),"resource_job_id":reservation.id});
        store.conn.execute("INSERT INTO reconciliation_checks(repo_id,reconciliation_id,data_json) VALUES(?1,?2,?3)",params![repo_id,r.id,receipt.to_string()])?;
        store.emit(repo_id,"reconciliation_acceptance",json!({"reconciliation_id":r.id,"stage":stage,"requirements_owner":owner,"validated_assignment":assignment,"head":head,"passed":passed&&unchanged,"duration_ms":started.elapsed().as_millis() as u64}))?;
        if !passed || !unchanged {
            bail!("{stage} acceptance validation failed for {owner}; receipt retained, shared reservations remain held");
        }
    }
    Ok(())
}

pub fn metrics(store: &Store, repo_id: i64, id: &str) -> Result<Value> {
    let r = get(store, repo_id, id)?;
    let mut timeline = Vec::new();
    let mut integration = Vec::new();
    let mut intent_revisions = 0;
    let mut invalidated_acknowledgements = 0;
    for event in store.events_since(repo_id, 0)? {
        let payload: Value = serde_json::from_str(&event.payload_json)?;
        if payload.get("reconciliation_id").and_then(Value::as_str) == Some(id) {
            if event.kind == "reconciliation_updated" {
                timeline.push(json!({"state":payload["state"],"revision":payload["revision"],"at":event.created_at}));
            }
            if event.kind == "reconciliation_intent_updated" {
                intent_revisions += 1;
            }
            invalidated_acknowledgements += payload
                .get("invalidated_stub_acknowledgements")
                .and_then(Value::as_u64)
                .unwrap_or(0);
        }
        if event.kind == "changeset_landed" {
            let assignment = payload.get("assignment_id").and_then(Value::as_str);
            if assignment == r.reconciler_assignment.as_deref()
                || r.participants
                    .iter()
                    .any(|p| Some(p.assignment_id.as_str()) == assignment)
            {
                integration.push(json!({"at":event.created_at,"changeset_id":payload["changeset_id"],"assignment_id":assignment}));
            }
        }
    }
    let first = |state: &str| {
        timeline
            .iter()
            .find(|v| v["state"] == state)
            .and_then(|v| v["at"].as_i64())
    };
    let elapsed = |start: Option<i64>, end: Option<i64>| -> Value {
        start
            .zip(end)
            .map(|(a, b)| json!((b - a).max(0)))
            .unwrap_or(Value::Null)
    };
    let created = timeline.first().and_then(|v| v["at"].as_i64());
    let mut q=store.conn.prepare("SELECT data_json FROM reconciliation_checks WHERE repo_id=?1 AND reconciliation_id=?2 ORDER BY id")?;
    let receipts: Vec<Value> = q
        .query_map(params![repo_id, id], |row| row.get::<_, String>(0))?
        .map(|row| Ok(serde_json::from_str(&row?)?))
        .collect::<Result<_>>()?;
    Ok(
        json!({"reconciliation_id":id,"state":r.state,"timeline":timeline,"seconds":{"request_to_first_ready":elapsed(created,first("ready")),"first_ready_to_dispatch":elapsed(first("ready"),first("dispatched")),"dispatch_to_return":elapsed(first("dispatched"),first("returned")),"return_to_both_consumers_validated":elapsed(first("returned"),first("completed"))},"contract_versions":r.contract.as_ref().map_or(0,|c|c.version),"intent_revisions":intent_revisions,"invalidated_stub_acknowledgements":invalidated_acknowledgements,"acceptance_runs":receipts.len(),"acceptance_failures":receipts.iter().filter(|v|v["passed"]==false).count(),"acceptance_receipts":receipts,"integration_events":integration,"measurement_limits":"Wall-clock state events use whole seconds. Contract versions and failed acceptance checks are rework signals, not a measured productivity improvement. Only explicit requirement tests are recorded; no speedup is inferred."}),
    )
}

pub fn publish(
    store: &Store,
    repo_id: i64,
    repo: &Path,
    cfg: &TowerConfig,
    id: &str,
    changeset: &str,
) -> Result<Reconciliation> {
    let mut r = get(store, repo_id, id)?;
    if r.state == "returned" && r.result_changeset.as_deref() == Some(changeset) {
        return Ok(r);
    }
    if r.state != "dispatched" {
        bail!("reconciliation is not dispatched");
    }
    ensure_frozen(store, repo_id, &r)?;
    let cs = proven_changeset(store, repo_id, repo, cfg, changeset)?;
    if r.reconciler_assignment.as_deref() != Some(&cs.assignment_id) {
        bail!("result belongs to a different assignment");
    }
    if !ancestor(repo, &r.base_commit, &cs.head_commit) {
        bail!("result does not preserve common base ancestry");
    }
    if let Some(c) = &r.contract {
        if !ancestor(repo, &c.stub_commit, &cs.head_commit) {
            bail!("result does not contain the provisional stub ancestry");
        }
        if fingerprints(repo, &c.stub_commit, &r.nodes)?
            == fingerprints(repo, &cs.head_commit, &r.nodes)?
        {
            bail!("result still contains the unchanged provisional stub; implement and validate the real shared behavior");
        }
    }
    ensure_commit_scope(repo, &r.base_commit, &cs.head_commit, &r.nodes)?;
    acceptance(
        store,
        repo_id,
        cfg,
        &r,
        "combined",
        &cs.assignment_id,
        &cs.head_commit,
    )?;
    ensure_frozen(store, repo_id, &r)?;
    proven_changeset(store, repo_id, repo, cfg, changeset)?;
    let tx = transaction(store)?;
    // CAS prevents a cancellation/second publication racing this validated result.
    r.state = "returned".into();
    r.result_commit = Some(cs.head_commit.clone());
    r.result_changeset = Some(cs.id);
    pin(repo, &r.id, "result", &cs.head_commit)?;
    save(store, repo_id, &mut r)?;
    store.emit(repo_id,"reconciliation_returned",json!({"reconciliation_id":r.id,"result_commit":r.result_commit,"participants":r.participants.iter().map(|p|&p.assignment_id).collect::<Vec<_>>(),"base_commit":r.base_commit,"contract":r.contract.as_ref().map(|c|json!({"version":c.version,"hash":c.hash}))}))?;
    commit_transaction(tx)?;
    Ok(r)
}

pub fn publish_ready(
    store: &Store,
    repo_id: i64,
    repo: &Path,
    cfg: &TowerConfig,
    task_id: &str,
    changeset: &str,
) -> Result<Option<Value>> {
    let Some(r) = list(store, repo_id)?
        .into_iter()
        .find(|r| r.task_id == task_id)
    else {
        return Ok(None);
    };
    Ok(Some(serde_json::to_value(publish(
        store, repo_id, repo, cfg, &r.id, changeset,
    )?)?))
}

pub fn resume(
    store: &Store,
    repo_id: i64,
    repo: &Path,
    cfg: &TowerConfig,
    id: &str,
    assignment: &str,
    changeset: &str,
) -> Result<Reconciliation> {
    let mut r = get(store, repo_id, id)?;
    if !matches!(r.state.as_str(), "returned" | "completed") {
        bail!("no verified combined result has been returned");
    }
    let p = r
        .participants
        .iter()
        .find(|p| p.assignment_id == assignment)
        .context("assignment is not a participant")?;
    let cs = proven_changeset(store, repo_id, repo, cfg, changeset)?;
    if cs.assignment_id != assignment {
        bail!("downstream validation belongs to another assignment");
    }
    if !ancestor(
        repo,
        p.paused_commit.as_ref().context("missing paused draft")?,
        &cs.head_commit,
    ) || !ancestor(
        repo,
        r.result_commit
            .as_ref()
            .context("missing combined result")?,
        &cs.head_commit,
    ) {
        bail!("downstream head must contain both original draft and exact returned result");
    }
    if let Some(c) = &r.contract {
        if p.stub_version != Some(c.version) {
            bail!("contract changed; provisional acknowledgement is stale");
        }
    }
    acceptance(
        store,
        repo_id,
        cfg,
        &r,
        "consumer",
        assignment,
        &cs.head_commit,
    )?;
    proven_changeset(store, repo_id, repo, cfg, changeset)?;
    let tx = transaction(store)?;
    let p = r
        .participants
        .iter_mut()
        .find(|p| p.assignment_id == assignment)
        .unwrap();
    p.resumed_head = Some(cs.head_commit);
    p.validation_changeset = Some(changeset.into());
    let done = r.participants.iter().all(|p| p.resumed_head.is_some());
    if done {
        for participant in &r.participants {
            let path = assignment_path(store, repo_id, &participant.assignment_id)?;
            clean_head(
                &path,
                participant
                    .resumed_head
                    .as_deref()
                    .context("missing downstream acknowledgement")?,
            )?;
            proven_changeset(
                store,
                repo_id,
                repo,
                cfg,
                participant
                    .validation_changeset
                    .as_deref()
                    .context("missing downstream validation")?,
            )?;
        }
        r.state = "completed".into();
        // No shared scope is returned to two writers. Release the reconciler's
        // claims only after both consumers validate; subsequent edits reacquire.
        if let Some(a) = &r.reconciler_assignment {
            store.release_claims_nodes(repo_id, a, Some(&r.nodes))?;
        }
    }
    save(store, repo_id, &mut r)?;
    commit_transaction(tx)?;
    Ok(r)
}

pub fn cancel(store: &Store, repo_id: i64, id: &str, reason: &str) -> Result<Reconciliation> {
    if reason.trim().is_empty() {
        bail!("cancellation needs a reason");
    }
    let tx = transaction(store)?;
    let mut r = get(store, repo_id, id)?;
    if r.state == "completed" {
        bail!("completed reconciliations cannot be cancelled");
    }
    // Cancellation after dispatch cannot prove that the launched worker stopped.
    // Fail closed: keep reservations and all pinned drafts for explicit recovery.
    r.state = if r.reconciler_assignment.is_some() || r.contract.is_some() {
        "cancelled_held"
    } else {
        "cancelled"
    }
    .into();
    r.reason = Some(reason.into());
    store.set_task_state(repo_id, &r.task_id, "failed")?;
    save(store, repo_id, &mut r)?;
    commit_transaction(tx)?;
    Ok(r)
}

/// Recovery is a new cooperative handoff. Each consumer explicitly restores the
/// pre-stub implementation while preserving its committed draft ancestry.
pub fn recovery_ack(
    store: &Store,
    repo_id: i64,
    id: &str,
    assignment: &str,
    head: &str,
) -> Result<Reconciliation> {
    let tx = transaction(store)?;
    let mut r = get(store, repo_id, id)?;
    if r.state != "cancelled_held" {
        bail!("recovery acknowledgement requires cancelled_held state");
    }
    let path = assignment_path(store, repo_id, assignment)?;
    let head = clean_head(&path, head)?;
    let p = r
        .participants
        .iter_mut()
        .find(|p| p.assignment_id == assignment)
        .context("assignment is not a participant")?;
    let draft = p.paused_commit.as_ref().unwrap_or(&p.draft_commit);
    if !ancestor(&path, draft, &head)
        && git::rev_parse(&path, &format!("{draft}^{{tree}}"))?
            != git::rev_parse(&path, "HEAD^{tree}")?
    {
        bail!("recovery must preserve the pinned original draft");
    }
    if fingerprints(&path, draft, &r.nodes)? != fingerprints(&path, &head, &r.nodes)? {
        bail!("restore the pre-stub shared implementation in a new commit before acknowledging recovery");
    }
    p.recovery_head = Some(head);
    save(store, repo_id, &mut r)?;
    commit_transaction(tx)?;
    Ok(r)
}

pub fn recover(store: &Store, repo_id: i64, id: &str) -> Result<Reconciliation> {
    let tx = transaction(store)?;
    let mut r = get(store, repo_id, id)?;
    if r.state != "cancelled_held" {
        bail!("recovery requires cancelled_held state");
    }
    for p in &r.participants {
        let path = assignment_path(store, repo_id, &p.assignment_id)?;
        clean_head(
            &path,
            p.recovery_head
                .as_deref()
                .context("both participants must acknowledge recovery first")?,
        )?;
    }
    // Database status alone is insufficient: verify each adapter's durable
    // terminal receipt and that it belongs to the expected worktree.
    let attempts: Vec<_> = crate::scheduler::runs(store, repo_id)?
        .into_iter()
        .filter(|a| a.task_id == r.task_id)
        .collect();
    for run in &attempts {
        let a = store
            .get_assignment(repo_id, &run.assignment_id)?
            .context("missing recovery assignment")?;
        if !crate::agent::quiescent(Path::new(&run.run_dir), Path::new(&a.worktree_path))? {
            bail!(
                "reconciler attempt {} is not proven stopped; reservations remain held",
                run.id
            );
        }
    }
    if attempts.is_empty() {
        if let Some(a) = &r.reconciler_assignment {
            let a = store
                .get_assignment(repo_id, a)?
                .context("missing reconciler")?;
            if a.state != "closed" {
                bail!("untracked reconciler assignment must be closed before recovery");
            }
        }
    }
    if let Some(a) = &r.reconciler_assignment {
        store.release_claims_nodes(repo_id, a, Some(&r.nodes))?;
    }
    // Restore only the claims actually transferred, never grant both originals
    // ownership of the shared implementation.
    for claim in &r.transferred_claims {
        let a = store
            .get_assignment(repo_id, &claim.assignment_id)?
            .context("missing original assignment")?;
        if a.state != "open" {
            bail!("original assignment closed; manual ownership review required");
        }
        let others = store.list_claims(repo_id)?;
        if others.iter().any(|c| {
            c.assignment_id != a.id
                && overlaps(
                    &claim.node,
                    &ClaimNode {
                        file: c.file_path.clone(),
                        item: c.item_name.clone(),
                    },
                )
        }) {
            bail!("shared claim changed during recovery; manual ownership review required");
        }
        if !others.iter().any(|c| {
            c.assignment_id == a.id
                && c.file_path == claim.node.file
                && c.item_name == claim.node.item
        }) {
            store.conn.execute("INSERT INTO claims(repo_id,assignment_id,file_path,item_name,created_at) VALUES(?1,?2,?3,?4,?5)",params![repo_id,a.id,claim.node.file,claim.node.item,now_secs()])?;
        }
    }
    r.state = "cancelled".into();
    save(store, repo_id, &mut r)?;
    commit_transaction(tx)?;
    Ok(r)
}

pub fn claim_blockers(
    store: &Store,
    repo_id: i64,
    assignment: &str,
    nodes: &[ClaimNode],
) -> Result<Vec<Value>> {
    let mut out = Vec::new();
    for r in list(store, repo_id)?.into_iter().filter(active) {
        if r.reconciler_assignment.as_deref() == Some(assignment) && r.state == "dispatched" {
            continue;
        }
        for n in nodes {
            if r.nodes.iter().any(|held| overlaps(held, n)) {
                out.push(json!({"assignment_id":r.reconciler_assignment.as_deref().unwrap_or(&r.id),"holding_assignment":r.reconciler_assignment.as_deref().unwrap_or(&r.id),"file":n.file,"item":n.item,"reconciliation_id":r.id,"reason":"shared scope reserved for cooperative reconciliation"}));
            }
        }
    }
    Ok(out)
}

/// Call at final landing, after validation remains available to consumers.
pub fn ensure_no_unresolved_stubs(store: &Store, repo_id: i64, assignment: &str) -> Result<()> {
    for r in list(store, repo_id)? {
        let participant = r.participants.iter().any(|p| p.assignment_id == assignment);
        let reconciler = r.reconciler_assignment.as_deref() == Some(assignment);
        if !participant && !reconciler {
            continue;
        }
        if active(&r) {
            bail!("assignment {assignment} has unresolved shared reconciliation {}; BOTH original assignments must acknowledge current downstream validation before any shared result lands",r.id);
        }
        if r.state == "completed" {
            let path = assignment_path(store, repo_id, assignment)?;
            let current = git::snapshot_commit(&path)?;
            let result = r
                .result_commit
                .as_ref()
                .context("completed reconciliation lacks result receipt")?;
            if reconciler && &current != result {
                bail!("reconciler assignment {assignment} is bound to immutable returned commit {result}; use a new task and ownership handoff for further changes");
            }
            if participant
                && (!ancestor(&path, result, &current)
                    || fingerprints(&path, result, &r.nodes)?
                        != fingerprints(&path, &current, &r.nodes)?)
            {
                bail!("assignment {assignment} changed the verified shared implementation from {}; use a new task and ownership handoff instead of landing stale reconciliation behavior",r.id);
            }
        }
    }
    Ok(())
}

/// Validation coverage is a receipt, not write ownership: a consumer may validate
/// the exact returned implementation while the shared write reservation remains.
/// Changing that implementation removes this coverage immediately.
pub fn returned_coverage(
    store: &Store,
    repo_id: i64,
    assignment: &str,
    head: &str,
) -> Result<Vec<ClaimNode>> {
    let mut out = Vec::new();
    for r in list(store, repo_id)?
        .into_iter()
        .filter(|r| matches!(r.state.as_str(), "returned" | "completed"))
    {
        if !r.participants.iter().any(|p| p.assignment_id == assignment) {
            continue;
        }
        let path = assignment_path(store, repo_id, assignment)?;
        let Some(result) = &r.result_commit else {
            continue;
        };
        if !ancestor(&path, result, head) {
            continue;
        }
        let expected = fingerprints(&path, result, &r.nodes)?;
        let actual = fingerprints(&path, head, &r.nodes)?;
        for (i, n) in r.nodes.iter().enumerate() {
            if expected[i] == actual[i] && !out.contains(n) {
                out.push(n.clone());
            }
        }
    }
    Ok(out)
}

/// Keep drafts visible on the board, but do not integrate them while a separate
/// task owns their shared implementation. Returned consumers rejoin once they
/// contain exactly the verified shared implementation.
pub fn changeset_suspended(
    store: &Store,
    repo_id: i64,
    assignment: &str,
    head: &str,
) -> Result<bool> {
    for r in list(store, repo_id)?.into_iter().filter(active) {
        if !r.participants.iter().any(|p| p.assignment_id == assignment) {
            continue;
        }
        if r.state != "returned" {
            return Ok(true);
        }
        let Some(result) = &r.result_commit else {
            return Ok(true);
        };
        let path = assignment_path(store, repo_id, assignment)?;
        if !ancestor(&path, result, head)
            || fingerprints(&path, result, &r.nodes)? != fingerprints(&path, head, &r.nodes)?
        {
            return Ok(true);
        }
    }
    Ok(false)
}

pub fn action_help() -> Value {
    json!({
        "discover":{},"list":{},"show":{"id":"R-1"},"metrics":{"id":"R-1"},
        "request":{"participants":[{"assignment_id":"A-1","intent":"explicit authored requirement","assumptions":["stable interface"],"acceptance_tests":["command exercising first requirement"]},{"assignment_id":"A-2","intent":"second authored requirement","assumptions":[],"acceptance_tests":["command exercising second requirement"]}],"nodes":[{"file":"src/shared.rs","item":"shared_function"}]},
        "intent":{"id":"R-1","assignment_id":"A-1","intent":"updated authored goal","assumptions":["explicit interface assumption"],"acceptance_tests":["explicit local acceptance command"]},
        "pause":{"id":"R-1","assignment_id":"A-1","head":"exact clean committed HEAD"},
        "contract":{"id":"R-1","interface":"explicit authored stable interface and provisional behavior","stub_commit":"immutable shared stub commit"},
        "stub-ack":{"id":"R-1","assignment_id":"A-1","head":"clean commit containing original draft and stub","version":1},
        "publish":{"id":"R-1","changeset_id":"C-1"},
        "resume":{"id":"R-1","assignment_id":"A-1","changeset_id":"current green downstream changeset containing original draft and result"},
        "cancel":{"id":"R-1","reason":"why reconciliation is cancelled"},
        "recovery-ack":{"id":"R-1","assignment_id":"A-1","head":"clean commit preserving paused draft and restoring pre-stub shared behavior"},
        "recover":{"id":"R-1"}
    })
}

/// Scope discovery is structural, not an assertion that two prose intents agree.
pub fn discover(store: &Store, repo_id: i64, repo: &Path, cfg: &TowerConfig) -> Result<Value> {
    let main = engine::main_head(repo, cfg)?;
    let reconciler_ids: Vec<_> = list(store, repo_id)?
        .iter()
        .filter_map(|r| r.reconciler_assignment.clone())
        .collect();
    let mut scopes = Vec::new();
    for a in store
        .list_assignments(repo_id)?
        .into_iter()
        .filter(|a| a.state == "open" && !reconciler_ids.contains(&a.id))
    {
        let path = assignment_path(store, repo_id, &a.id)?;
        let head = git::rev_parse(&path, "HEAD")?;
        let base = git::merge_base(&path, &main, &head)?;
        let (files, _, changed) = engine::live_diff(repo, &path, &base)?;
        let mut nodes = Vec::new();
        for file in files {
            let names: Vec<_> = changed
                .iter()
                .filter(|(f, _)| f == &file)
                .map(|(_, n)| n.clone())
                .collect();
            if names.is_empty() {
                nodes.push(ClaimNode { file, item: None });
            } else {
                for name in names {
                    nodes.push(ClaimNode {
                        file: file.clone(),
                        item: Some(name),
                    });
                }
            }
        }
        scopes.push((a, nodes, git::snapshot_commit(&path)?));
    }
    let mut overlaps_found = Vec::new();
    for i in 0..scopes.len() {
        for j in i + 1..scopes.len() {
            let mut nodes = Vec::new();
            for a in &scopes[i].1 {
                for b in &scopes[j].1 {
                    if overlaps(a, b) {
                        let n = if a.item.is_none() || b.item.is_none() {
                            ClaimNode {
                                file: a.file.clone(),
                                item: None,
                            }
                        } else {
                            a.clone()
                        };
                        if fingerprints(repo, &scopes[i].2, std::slice::from_ref(&n))?
                            == fingerprints(repo, &scopes[j].2, std::slice::from_ref(&n))?
                        {
                            continue;
                        }
                        if !nodes.contains(&n) {
                            nodes.push(n);
                        }
                    }
                }
            }
            if !nodes.is_empty() {
                overlaps_found.push(json!({"assignments":[scopes[i].0.id,scopes[j].0.id],"nodes":nodes,"requires":"both authored intents and cooperative handoff","semantic_merge_proven":false}));
            }
        }
    }
    Ok(json!(overlaps_found))
}

fn field<'a>(v: &'a Value, key: &str) -> Result<&'a str> {
    v.get(key)
        .and_then(Value::as_str)
        .with_context(|| format!("missing string field {key}"))
}

/// Shared JSON action surface for CLI and MCP; arguments are deliberately the same.
pub fn dispatch(
    store: &Store,
    repo_id: i64,
    repo: &Path,
    cfg: &TowerConfig,
    action: &str,
    input: &Value,
) -> Result<Value> {
    migrate(store)?;
    match action {
        "help" => Ok(action_help()),
        "discover" => discover(store, repo_id, repo, cfg),
        "list" => Ok(serde_json::to_value(list(store, repo_id)?)?),
        "show" => Ok(serde_json::to_value(get(
            store,
            repo_id,
            field(input, "id")?,
        )?)?),
        "metrics" => metrics(store, repo_id, field(input, "id")?),
        "intent" => Ok(serde_json::to_value(update_intent(
            store,
            repo_id,
            field(input, "id")?,
            serde_json::from_value(input.clone())?,
        )?)?),
        "request" => Ok(serde_json::to_value(request(
            store,
            repo_id,
            serde_json::from_value(
                input
                    .get("participants")
                    .context("missing participants")?
                    .clone(),
            )?,
            serde_json::from_value(input.get("nodes").context("missing nodes")?.clone())?,
        )?)?),
        "pause" => Ok(serde_json::to_value(pause(
            store,
            repo_id,
            field(input, "id")?,
            field(input, "assignment_id")?,
            field(input, "head")?,
        )?)?),
        "contract" => Ok(serde_json::to_value(contract_set(
            store,
            repo_id,
            field(input, "id")?,
            field(input, "interface")?,
            field(input, "stub_commit")?,
        )?)?),
        "stub-ack" => Ok(serde_json::to_value(stub_ack(
            store,
            repo_id,
            field(input, "id")?,
            field(input, "assignment_id")?,
            field(input, "head")?,
            input
                .get("version")
                .and_then(Value::as_u64)
                .and_then(|v| u32::try_from(v).ok())
                .context("missing u32 version")?,
        )?)?),
        "publish" => Ok(serde_json::to_value(publish(
            store,
            repo_id,
            repo,
            cfg,
            field(input, "id")?,
            field(input, "changeset_id")?,
        )?)?),
        "resume" => Ok(serde_json::to_value(resume(
            store,
            repo_id,
            repo,
            cfg,
            field(input, "id")?,
            field(input, "assignment_id")?,
            field(input, "changeset_id")?,
        )?)?),
        "cancel" => Ok(serde_json::to_value(cancel(
            store,
            repo_id,
            field(input, "id")?,
            field(input, "reason")?,
        )?)?),
        "recovery-ack" => Ok(serde_json::to_value(recovery_ack(
            store,
            repo_id,
            field(input, "id")?,
            field(input, "assignment_id")?,
            field(input, "head")?,
        )?)?),
        "recover" => Ok(serde_json::to_value(recover(
            store,
            repo_id,
            field(input, "id")?,
        )?)?),
        _ => bail!("unknown reconciliation action {action}"),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::path::PathBuf;

    const BASE:&str="export function shared(): number {\n  return 0;\n}\nexport function independent(): number {\n  return 10;\n}\n";
    fn run(path: &Path, args: &[&str]) {
        git::run_git(path, args).unwrap();
    }
    fn write(path: &Path, text: &str) {
        std::fs::create_dir_all(path.parent().unwrap()).unwrap();
        std::fs::write(path, text).unwrap();
    }
    fn save_all(path: &Path) {
        run(path, &["add", "-A"]);
        run(path, &["commit", "-m", "fixture change"]);
    }
    fn head(path: &Path) -> String {
        git::rev_parse(path, "HEAD").unwrap()
    }
    fn shared(value: i32) -> String {
        BASE.replace("return 0;", &format!("return {value};"))
    }
    fn node() -> ClaimNode {
        ClaimNode {
            file: "src/shared.ts".into(),
            item: Some("shared".into()),
        }
    }
    struct Fixture {
        temp: tempfile::TempDir,
        repo: PathBuf,
        store: Store,
        rid: i64,
        cfg: TowerConfig,
        a: Vec<String>,
        paths: Vec<PathBuf>,
    }
    impl Fixture {
        fn new() -> Self {
            let temp = tempfile::tempdir().unwrap();
            let repo = temp.path().join("repo");
            std::fs::create_dir_all(&repo).unwrap();
            run(&repo, &["init", "-b", "main"]);
            run(&repo, &["config", "user.name", "Test"]);
            run(&repo, &["config", "user.email", "test@local"]);
            write(&repo.join("src/shared.ts"), BASE);
            write(&repo.join("src/a.ts"), "export const a = 0;\n");
            write(&repo.join("src/b.ts"), "export const b = 0;\n");
            write(&repo.join("tower.toml"),"[repo]\nmain_branch=\"main\"\n[gates]\ncheck=\"grep -q 'return 3;' src/shared.ts\"\ntest=\"grep -q 'return 3;' src/shared.ts\"\n[feedback]\nenabled=false\n");
            save_all(&repo);
            let store = Store::open(temp.path().join("state")).unwrap();
            let rid = store
                .register_repo(&git::canonical_str(&repo), "main")
                .unwrap()
                .id;
            let cfg = TowerConfig::load(&repo);
            let mut a = Vec::new();
            let mut paths = Vec::new();
            for n in 0..2 {
                let path = temp.path().join(format!("worker{n}"));
                let branch = format!("worker{n}");
                run(
                    &repo,
                    &[
                        "worktree",
                        "add",
                        "-b",
                        &branch,
                        path.to_str().unwrap(),
                        "main",
                    ],
                );
                let task = store
                    .create_task(
                        rid,
                        &format!("worker {n}"),
                        &format!("shared must include contribution {}", n + 1),
                    )
                    .unwrap();
                let assignment = store
                    .create_assignment(rid, &task.id, path.to_str().unwrap(), &branch)
                    .unwrap();
                a.push(assignment.id);
                paths.push(path);
            }
            Self {
                temp,
                repo,
                store,
                rid,
                cfg,
                a,
                paths,
            }
        }
        fn draft(&self, n: usize, value: i32) {
            write(&self.paths[n].join("src/shared.ts"), &shared(value));
            let name = if n == 0 { "a" } else { "b" };
            write(
                &self.paths[n].join(format!("src/{name}.ts")),
                &format!("export const {name} = 1;\n"),
            );
            save_all(&self.paths[n]);
        }
        fn request(&self) -> Reconciliation {
            request(
                &self.store,
                self.rid,
                self.a
                    .iter()
                    .enumerate()
                    .map(|(n, a)| Intent {
                        assignment_id: a.clone(),
                        intent: format!("include contribution {} in shared return", n + 1),
                        assumptions: vec!["shared() -> number".into()],
                        acceptance_tests: vec!["grep -q 'return 3;' src/shared.ts".into()],
                    })
                    .collect(),
                vec![node()],
            )
            .unwrap()
        }
        fn paused(&self) -> Reconciliation {
            let r = self.request();
            for n in 0..2 {
                pause(
                    &self.store,
                    self.rid,
                    &r.id,
                    &self.a[n],
                    &head(&self.paths[n]),
                )
                .unwrap();
            }
            get(&self.store, self.rid, &r.id).unwrap()
        }
        fn reconciler(&self, r: &Reconciliation) -> (String, PathBuf) {
            let path = self.temp.path().join("reconciler");
            run(
                &self.repo,
                &[
                    "worktree",
                    "add",
                    "-b",
                    "reconciler",
                    path.to_str().unwrap(),
                    "main",
                ],
            );
            let a = self
                .store
                .create_assignment(self.rid, &r.task_id, path.to_str().unwrap(), "reconciler")
                .unwrap();
            (a.id, path)
        }
        fn register(&self, assignment: &str, path: &Path) -> String {
            engine::register_changeset(
                &self.store,
                self.rid,
                &self.repo,
                &self.cfg,
                assignment,
                path.to_str().unwrap(),
            )
            .unwrap()["changeset_id"]
                .as_str()
                .unwrap()
                .into()
        }
        fn gates(&self, c: &str) {
            for tier in [1, 2] {
                let receipt =
                    engine::gate_run_single(&self.store, self.rid, &self.repo, &self.cfg, c, tier)
                        .unwrap();
                assert_eq!(receipt["passed"], true, "{receipt}");
            }
        }
        fn merge_result(&self, n: usize, result: &str) {
            let _ = git::run_git_raw(&self.paths[n], &["merge", "--no-commit", result]);
            write(&self.paths[n].join("src/shared.ts"), &shared(3));
            save_all(&self.paths[n]);
        }
    }

    #[test]
    fn structural_discovery_captures_both_intents_dedupes_and_pins_dirty_drafts() {
        let f = Fixture::new();
        f.draft(0, 1);
        f.draft(1, 2);
        let found = discover(&f.store, f.rid, &f.repo, &f.cfg).unwrap();
        assert_eq!(found.as_array().unwrap().len(), 1);
        assert_eq!(found[0]["nodes"][0]["item"], "shared");
        write(&f.paths[0].join("src/a.ts"), "export const a = 99;\n");
        let status = git::run_git(&f.paths[0], &["status", "--porcelain"]).unwrap();
        let r = f.request();
        assert_eq!(r.participants.len(), 2);
        assert!(r.participants[0].intent.contains("contribution 1"));
        assert_eq!(f.request().id, r.id);
        assert_eq!(
            git::run_git(&f.paths[0], &["status", "--porcelain"]).unwrap(),
            status
        );
        assert_eq!(
            git::rev_parse(
                &f.repo,
                &format!("refs/tower/reconciliations/{}/draft-0", r.id)
            )
            .unwrap(),
            r.participants[0].draft_commit
        );
        assert!(pause(&f.store, f.rid, &r.id, &f.a[0], &head(&f.paths[0])).is_err());
        assert!(!scheduler_task_ready(&f.store, f.rid, &r.task_id).unwrap());
        assert!(!claim_blockers(&f.store, f.rid, &f.a[0], &[node()])
            .unwrap()
            .is_empty());
        let cancel = cancel(&f.store, f.rid, &r.id, "requirements changed").unwrap();
        assert_eq!(cancel.state, "cancelled");
        assert!(claim_blockers(&f.store, f.rid, &f.a[0], &[node()])
            .unwrap()
            .is_empty());
    }

    #[test]
    fn ownership_transfer_preserves_independent_claims_and_recovers_after_restart() {
        let f = Fixture::new();
        f.draft(0, 1);
        f.draft(1, 2);
        let own = ClaimNode {
            file: "src/a.ts".into(),
            item: None,
        };
        f.store
            .acquire_claims_partial(f.rid, &f.a[0], &[node(), own.clone()])
            .unwrap();
        let r = f.paused();
        let (agent, path) = f.reconciler(&r);
        // Independent work can continue in another function while shared source stays frozen.
        let text = std::fs::read_to_string(f.paths[0].join("src/shared.ts"))
            .unwrap()
            .replace("return 10;", "return 11;");
        write(&f.paths[0].join("src/shared.ts"), &text);
        let brief = prepare_dispatch(&f.store, f.rid, &r.task_id, &agent)
            .unwrap()
            .unwrap();
        assert!(brief.contains("contribution 1") && brief.contains("contribution 2"));
        assert_eq!(f.store.claims_for(f.rid, &f.a[0]).unwrap(), vec![own]);
        assert_eq!(f.store.claims_for(f.rid, &agent).unwrap(), vec![node()]);
        assert!(claim_blockers(&f.store, f.rid, &agent, &[node()])
            .unwrap()
            .is_empty());
        assert!(prepare_dispatch(&f.store, f.rid, &r.task_id, &agent)
            .unwrap()
            .is_some());
        let reopened = Store::open(f.temp.path().join("state")).unwrap();
        assert_eq!(get(&reopened, f.rid, &r.id).unwrap().state, "dispatched");
        assert_eq!(
            get(&reopened, f.rid, &r.id)
                .unwrap()
                .reconciler_assignment
                .as_deref(),
            Some(agent.as_str())
        );
        assert!(path.exists());
    }

    #[test]
    fn stale_shared_edit_refuses_transfer_without_losing_any_claim() {
        let f = Fixture::new();
        f.draft(0, 1);
        f.draft(1, 2);
        f.store
            .acquire_claims_partial(f.rid, &f.a[0], &[node()])
            .unwrap();
        let r = f.paused();
        let (agent, _) = f.reconciler(&r);
        write(&f.paths[0].join("src/shared.ts"), &shared(9));
        assert!(prepare_dispatch(&f.store, f.rid, &r.task_id, &agent).is_err());
        assert_eq!(f.store.claims_for(f.rid, &f.a[0]).unwrap(), vec![node()]);
        assert!(f.store.claims_for(f.rid, &agent).unwrap().is_empty());
        assert_eq!(get(&f.store, f.rid, &r.id).unwrap().state, "ready");
    }

    #[test]
    fn combined_result_gates_returns_validates_both_consumers_and_avoids_loop() {
        let f = Fixture::new();
        f.draft(0, 1);
        f.draft(1, 2);
        f.store
            .acquire_claims_partial(
                f.rid,
                &f.a[0],
                &[
                    node(),
                    ClaimNode {
                        file: "src/a.ts".into(),
                        item: None,
                    },
                ],
            )
            .unwrap();
        f.store
            .acquire_claims_partial(
                f.rid,
                &f.a[1],
                &[ClaimNode {
                    file: "src/b.ts".into(),
                    item: None,
                }],
            )
            .unwrap();
        let old_a = f.register(&f.a[0], &f.paths[0]);
        let old_b = f.register(&f.a[1], &f.paths[1]);
        let r = f.paused();
        let (agent, path) = f.reconciler(&r);
        prepare_dispatch(&f.store, f.rid, &r.task_id, &agent).unwrap();
        assert!(engine::integration_order(&f.store, f.rid)
            .unwrap()
            .is_empty());
        write(&path.join("src/shared.ts"), &shared(3));
        save_all(&path);
        let result = f.register(&agent, &path);
        assert!(publish(&f.store, f.rid, &f.repo, &f.cfg, &r.id, &result).is_err());
        f.gates(&result);
        let returned = publish(&f.store, f.rid, &f.repo, &f.cfg, &r.id, &result).unwrap();
        assert_eq!(returned.state, "returned");
        assert!(ensure_no_unresolved_stubs(&f.store, f.rid, &agent).is_err());
        assert!(changeset_suspended(&f.store, f.rid, &f.a[0], &head(&f.paths[0])).unwrap());
        let result_head = returned.result_commit.as_ref().unwrap();
        for n in 0..2 {
            f.merge_result(n, result_head);
            assert_eq!(
                returned_coverage(&f.store, f.rid, &f.a[n], &head(&f.paths[n])).unwrap(),
                vec![node()]
            );
            assert!(!changeset_suspended(&f.store, f.rid, &f.a[n], &head(&f.paths[n])).unwrap());
            let consumer = f.register(&f.a[n], &f.paths[n]);
            assert!(resume(&f.store, f.rid, &f.repo, &f.cfg, &r.id, &f.a[n], &consumer).is_err());
            f.gates(&consumer);
            let state =
                resume(&f.store, f.rid, &f.repo, &f.cfg, &r.id, &f.a[n], &consumer).unwrap();
            assert_eq!(state.state, if n == 0 { "returned" } else { "completed" });
            if n == 0 {
                assert!(ensure_no_unresolved_stubs(&f.store, f.rid, &f.a[n]).is_err());
            }
        }
        assert!(ensure_no_unresolved_stubs(&f.store, f.rid, &agent).is_ok());
        assert!(claim_blockers(&f.store, f.rid, &f.a[0], &[node()])
            .unwrap()
            .is_empty());
        assert!(f.store.claims_for(f.rid, &agent).unwrap().is_empty());
        assert!(discover(&f.store, f.rid, &f.repo, &f.cfg)
            .unwrap()
            .as_array()
            .unwrap()
            .is_empty());
        assert!(
            f.store
                .get_changeset(f.rid, &old_a)
                .unwrap()
                .unwrap()
                .superseded
        );
        assert!(
            f.store
                .get_changeset(f.rid, &old_b)
                .unwrap()
                .unwrap()
                .superseded
        );
    }

    #[test]
    fn contract_versions_invalidate_acknowledgements_and_cancel_recovery_restores_owner() {
        let f = Fixture::new();
        f.store
            .acquire_claims_partial(f.rid, &f.a[0], &[node()])
            .unwrap();
        let r = f.paused();
        let stub = f.temp.path().join("stub");
        run(
            &f.repo,
            &[
                "worktree",
                "add",
                "-b",
                "stub",
                stub.to_str().unwrap(),
                "main",
            ],
        );
        write(&stub.join("src/shared.ts"), &shared(-1));
        save_all(&stub);
        let stub_head = head(&stub);
        let r = contract_set(
            &f.store,
            f.rid,
            &r.id,
            "shared() -> number; -1 is a provisional sentinel",
            &stub_head,
        )
        .unwrap();
        for n in 0..2 {
            run(&f.paths[n], &["merge", "--no-edit", &stub_head]);
            stub_ack(&f.store, f.rid, &r.id, &f.a[n], &head(&f.paths[n]), 1).unwrap();
        }
        assert_eq!(get(&f.store, f.rid, &r.id).unwrap().state, "ready");
        contract_set(
            &f.store,
            f.rid,
            &r.id,
            "shared() -> number; real behavior must return combined needs",
            &stub_head,
        )
        .unwrap();
        assert!(stub_ack(&f.store, f.rid, &r.id, &f.a[0], &head(&f.paths[0]), 1).is_err());
        assert_eq!(
            get(&f.store, f.rid, &r.id).unwrap().state,
            "awaiting_handoffs"
        );
        for n in 0..2 {
            stub_ack(&f.store, f.rid, &r.id, &f.a[n], &head(&f.paths[n]), 2).unwrap();
        }
        let r = get(&f.store, f.rid, &r.id).unwrap();
        let (agent, _) = f.reconciler(&r);
        prepare_dispatch(&f.store, f.rid, &r.task_id, &agent).unwrap();
        assert_eq!(
            cancel(&f.store, f.rid, &r.id, "cancel provisional implementation")
                .unwrap()
                .state,
            "cancelled_held"
        );
        assert!(recover(&f.store, f.rid, &r.id).is_err());
        for n in 0..2 {
            assert!(recovery_ack(&f.store, f.rid, &r.id, &f.a[n], &head(&f.paths[n])).is_err());
            write(&f.paths[n].join("src/shared.ts"), BASE);
            save_all(&f.paths[n]);
            recovery_ack(&f.store, f.rid, &r.id, &f.a[n], &head(&f.paths[n])).unwrap();
        }
        assert!(
            recover(&f.store, f.rid, &r.id).is_err(),
            "untracked open reconciler remains unsafe"
        );
        f.store.close_assignment(f.rid, &agent).unwrap();
        assert_eq!(recover(&f.store, f.rid, &r.id).unwrap().state, "cancelled");
        assert_eq!(f.store.claims_for(f.rid, &f.a[0]).unwrap(), vec![node()]);
        assert!(f.store.claims_for(f.rid, &f.a[1]).unwrap().is_empty());
    }

    #[test]
    fn unchanged_provisional_stub_never_publishes_even_if_a_weak_gate_passes() {
        let mut f = Fixture::new();
        // Deliberately weak validation models a misconfigured repository. The
        // protocol must still reject an unchanged provisional implementation.
        f.cfg.gates.check = Some("test -f src/shared.ts".into());
        f.cfg.gates.test = None;
        let r = f.paused();
        let stub = f.temp.path().join("stub");
        run(
            &f.repo,
            &[
                "worktree",
                "add",
                "-b",
                "stub",
                stub.to_str().unwrap(),
                "main",
            ],
        );
        write(&stub.join("src/shared.ts"), &shared(-1));
        save_all(&stub);
        let stub_head = head(&stub);
        contract_set(
            &f.store,
            f.rid,
            &r.id,
            "shared returns a number; -1 is temporary",
            &stub_head,
        )
        .unwrap();
        for n in 0..2 {
            run(&f.paths[n], &["merge", "--no-edit", &stub_head]);
            stub_ack(&f.store, f.rid, &r.id, &f.a[n], &head(&f.paths[n]), 1).unwrap();
        }
        let r = get(&f.store, f.rid, &r.id).unwrap();
        let (agent, path) = f.reconciler(&r);
        prepare_dispatch(&f.store, f.rid, &r.task_id, &agent).unwrap();
        run(&path, &["merge", "--no-edit", &stub_head]);
        let c = f.register(&agent, &path);
        assert_eq!(
            engine::gate_run_single(&f.store, f.rid, &f.repo, &f.cfg, &c, 1).unwrap()["passed"],
            true
        );
        let err = publish(&f.store, f.rid, &f.repo, &f.cfg, &r.id, &c)
            .unwrap_err()
            .to_string();
        assert!(err.contains("unchanged provisional stub"), "{err}");
        assert_eq!(get(&f.store, f.rid, &r.id).unwrap().state, "dispatched");
    }

    #[test]
    fn provisional_consumers_continue_independent_work_then_validate_real_behavior() {
        let f = Fixture::new();
        f.store
            .acquire_claims_partial(
                f.rid,
                &f.a[0],
                &[ClaimNode {
                    file: "src/a.ts".into(),
                    item: None,
                }],
            )
            .unwrap();
        f.store
            .acquire_claims_partial(
                f.rid,
                &f.a[1],
                &[ClaimNode {
                    file: "src/b.ts".into(),
                    item: None,
                }],
            )
            .unwrap();
        let r = f.paused();
        let stub = f.temp.path().join("stub");
        run(
            &f.repo,
            &[
                "worktree",
                "add",
                "-b",
                "stub",
                stub.to_str().unwrap(),
                "main",
            ],
        );
        write(&stub.join("src/shared.ts"), &shared(-1));
        save_all(&stub);
        let stub_head = head(&stub);
        contract_set(
            &f.store,
            f.rid,
            &r.id,
            "shared() -> number; -1 unresolved until contributions 1+2 return 3",
            &stub_head,
        )
        .unwrap();
        for n in 0..2 {
            run(&f.paths[n], &["merge", "--no-edit", &stub_head]);
            stub_ack(&f.store, f.rid, &r.id, &f.a[n], &head(&f.paths[n]), 1).unwrap();
        }
        let r = get(&f.store, f.rid, &r.id).unwrap();
        let (agent, path) = f.reconciler(&r);
        prepare_dispatch(&f.store, f.rid, &r.task_id, &agent).unwrap();
        for n in 0..2 {
            let file = if n == 0 { "src/a.ts" } else { "src/b.ts" };
            write(
                &f.paths[n].join(file),
                &format!("export const independently_built = {};\n", n + 5),
            );
            save_all(&f.paths[n]);
            assert!(ensure_no_unresolved_stubs(&f.store, f.rid, &f.a[n]).is_err());
        }
        run(&path, &["merge", "--no-edit", &stub_head]);
        write(&path.join("src/shared.ts"), &shared(3));
        save_all(&path);
        let c = f.register(&agent, &path);
        f.gates(&c);
        let r = publish(&f.store, f.rid, &f.repo, &f.cfg, &r.id, &c).unwrap();
        for n in 0..2 {
            f.merge_result(n, r.result_commit.as_deref().unwrap());
            let c = f.register(&f.a[n], &f.paths[n]);
            f.gates(&c);
            resume(&f.store, f.rid, &f.repo, &f.cfg, &r.id, &f.a[n], &c).unwrap();
        }
        assert_eq!(get(&f.store, f.rid, &r.id).unwrap().state, "completed");
        assert!(std::fs::read_to_string(f.paths[0].join("src/a.ts"))
            .unwrap()
            .contains("= 5"));
        assert!(std::fs::read_to_string(f.paths[1].join("src/b.ts"))
            .unwrap()
            .contains("= 6"));
        assert_eq!(
            engine::land_changeset(&f.store, f.rid, &f.repo, &f.cfg, &c).unwrap()["landed"],
            true
        );
        let landed = head(&f.repo);
        // A completed receipt is not reusable authorization to reintroduce a
        // stub under the same reconciler assignment, even with weak new gates.
        write(&path.join("src/shared.ts"), &shared(-1));
        save_all(&path);
        let mut weak = f.cfg.clone();
        weak.gates.check = Some("test -f src/shared.ts".into());
        weak.gates.test = None;
        let changed = engine::register_changeset(
            &f.store,
            f.rid,
            &f.repo,
            &weak,
            &agent,
            path.to_str().unwrap(),
        )
        .unwrap()["changeset_id"]
            .as_str()
            .unwrap()
            .to_string();
        assert_eq!(
            engine::gate_run_single(&f.store, f.rid, &f.repo, &weak, &changed, 1).unwrap()
                ["passed"],
            true
        );
        let error = engine::land_changeset(&f.store, f.rid, &f.repo, &weak, &changed)
            .unwrap_err()
            .to_string();
        assert!(error.contains("immutable returned commit"), "{error}");
        assert_eq!(head(&f.repo), landed);
        write(&f.paths[0].join("src/shared.ts"), &shared(-1));
        save_all(&f.paths[0]);
        assert!(ensure_no_unresolved_stubs(&f.store, f.rid, &f.a[0])
            .unwrap_err()
            .to_string()
            .contains("verified shared implementation"));
    }

    #[test]
    fn unindexed_or_adjacent_edits_need_conservative_whole_file_handoff() {
        let f = Fixture::new();
        let base = head(&f.repo);
        write(
            &f.paths[0].join("src/shared.ts"),
            &shared(3).replace("return 10;", "return 11;"),
        );
        save_all(&f.paths[0]);
        assert!(ensure_commit_scope(&f.repo, &base, &head(&f.paths[0]), &[node()]).is_err());
        assert!(ensure_commit_scope(
            &f.repo,
            &base,
            &head(&f.paths[0]),
            &[ClaimNode {
                file: "src/shared.ts".into(),
                item: None
            }]
        )
        .is_ok());
        f.store
            .acquire_claims_partial(
                f.rid,
                &f.a[0],
                &[ClaimNode {
                    file: "src/shared.ts".into(),
                    item: None,
                }],
            )
            .unwrap();
        assert!(
            f.request().nodes[0].item.is_none(),
            "existing broad ownership widens the transfer without silently splitting the claim"
        );
    }

    #[test]
    fn missing_criteria_wait_and_failed_acceptance_records_rework_without_return() {
        let f = Fixture::new();
        f.draft(0, 1);
        f.draft(1, 2);
        let r = request(
            &f.store,
            f.rid,
            vec![
                Intent {
                    assignment_id: f.a[0].clone(),
                    intent: "need contribution 1".into(),
                    ..Default::default()
                },
                Intent {
                    assignment_id: f.a[1].clone(),
                    intent: "need contribution 2".into(),
                    ..Default::default()
                },
            ],
            vec![node()],
        )
        .unwrap();
        for n in 0..2 {
            pause(&f.store, f.rid, &r.id, &f.a[n], &head(&f.paths[n])).unwrap();
        }
        assert!(!scheduler_task_ready(&f.store, f.rid, &r.task_id).unwrap());
        for n in 0..2 {
            update_intent(
                &f.store,
                f.rid,
                &r.id,
                Intent {
                    assignment_id: f.a[n].clone(),
                    intent: format!("include contribution {}", n + 1),
                    assumptions: vec!["number result".into()],
                    acceptance_tests: vec![if n == 0 {
                        "grep -q 'return 3;' src/shared.ts".into()
                    } else {
                        "grep -q 'deliberately missing requirement' src/shared.ts".into()
                    }],
                },
            )
            .unwrap();
            pause(&f.store, f.rid, &r.id, &f.a[n], &head(&f.paths[n])).unwrap();
        }
        let r = get(&f.store, f.rid, &r.id).unwrap();
        let (agent, path) = f.reconciler(&r);
        prepare_dispatch(&f.store, f.rid, &r.task_id, &agent).unwrap();
        write(&path.join("src/shared.ts"), &shared(3));
        save_all(&path);
        let c = f.register(&agent, &path);
        f.gates(&c);
        let err = publish(&f.store, f.rid, &f.repo, &f.cfg, &r.id, &c)
            .unwrap_err()
            .to_string();
        assert!(err.contains("acceptance validation failed"), "{err}");
        assert_eq!(get(&f.store, f.rid, &r.id).unwrap().state, "dispatched");
        let data = metrics(&f.store, f.rid, &r.id).unwrap();
        assert_eq!(data["acceptance_runs"], 2);
        assert_eq!(data["acceptance_failures"], 1);
        assert!(data["seconds"]["dispatch_to_return"].is_null());
        assert!(data["acceptance_receipts"][0]["tree_hash"]
            .as_str()
            .is_some());
        assert!(ensure_no_unresolved_stubs(&f.store, f.rid, &agent).is_err());
    }

    #[test]
    fn stub_ack_refuses_ancestry_with_a_different_shared_merge_resolution() {
        let f = Fixture::new();
        f.draft(0, 1);
        f.draft(1, 2);
        let r = f.paused();
        let stub = f.temp.path().join("stub");
        run(
            &f.repo,
            &[
                "worktree",
                "add",
                "-b",
                "stub",
                stub.to_str().unwrap(),
                "main",
            ],
        );
        write(&stub.join("src/shared.ts"), &shared(-1));
        save_all(&stub);
        let stub_head = head(&stub);
        contract_set(
            &f.store,
            f.rid,
            &r.id,
            "shared() -> number, -1 provisional",
            &stub_head,
        )
        .unwrap();
        let _ = git::run_git_raw(&f.paths[0], &["merge", "--no-commit", &stub_head]);
        write(&f.paths[0].join("src/shared.ts"), &shared(1));
        save_all(&f.paths[0]);
        assert!(ancestor(&f.repo, &stub_head, &head(&f.paths[0])));
        let error = stub_ack(&f.store, f.rid, &r.id, &f.a[0], &head(&f.paths[0]), 1)
            .unwrap_err()
            .to_string();
        assert!(
            error.contains("exact shared provisional implementation"),
            "{error}"
        );
        write(&f.paths[0].join("src/shared.ts"), &shared(-1));
        save_all(&f.paths[0]);
        assert!(stub_ack(&f.store, f.rid, &r.id, &f.a[0], &head(&f.paths[0]), 1).is_ok());
    }

    #[test]
    fn dirty_snapshot_handoff_preserves_shared_draft_while_advancing_independent_work() {
        let f = Fixture::new();
        write(&f.paths[0].join("src/shared.ts"), &shared(1));
        let r = f.request();
        write(&f.paths[0].join("src/a.ts"), "export const a = 42;\n");
        save_all(&f.paths[0]);
        assert!(pause(&f.store, f.rid, &r.id, &f.a[0], &head(&f.paths[0])).is_ok());
        assert_eq!(
            git::rev_parse(
                &f.repo,
                &format!("refs/tower/reconciliations/{}/draft-0", r.id)
            )
            .unwrap(),
            r.participants[0].draft_commit
        );
    }
}
