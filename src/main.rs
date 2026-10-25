mod board;
mod config;
mod engine;
mod git;
mod index;
mod mcp;
mod ops;
mod store;
#[cfg(test)]
mod tests;

use anyhow::{bail, Context, Result};
use clap::{Parser, Subcommand};
use std::io::IsTerminal;
use std::path::{Path, PathBuf};

use config::TowerConfig;
use store::{ClaimNode, Store};

#[derive(Parser)]
#[command(name = "tower", about = "Tower — local orchestration for parallel agent work in git worktrees")]
struct Cli {
    /// Repo path (default: current directory)
    #[arg(long, global = true)]
    repo: Option<PathBuf>,
    #[command(subcommand)]
    command: Command,
}

#[derive(Subcommand)]
enum Command {
    /// Register this repo; writes default tower.toml only with --write-config (or interactive yes)
    Init { #[arg(long)] write_config: bool, #[arg(long)] main_branch: Option<String> },
    /// Print the board
    Board { #[arg(long)] html: Option<PathBuf>, #[arg(long)] json: bool },
    /// Polling radar loop (default every 2s)
    Watch { #[arg(long, default_value_t = 2)] interval: u64, #[arg(long)] once: bool, #[arg(long)] iterations: Option<usize>, #[arg(long)] run_tier1: bool },
    Task { #[command(subcommand)] cmd: TaskCmd },
    Assignment { #[command(subcommand)] cmd: AssignmentCmd },
    Claim { #[command(subcommand)] cmd: ClaimCmd },
    Worktree { #[command(subcommand)] cmd: WorktreeCmd },
    Changeset { #[command(subcommand)] cmd: ChangesetCmd },
    Gate { #[command(subcommand)] cmd: GateCmd },
    /// Run simulation + rehearsal now, print results
    MergeCheck,
    /// Print planned landing order
    Order,
    Escalate { #[command(subcommand)] cmd: EscalateCmd },
    Decisions { #[command(subcommand)] cmd: DecisionsCmd },
    Events { #[arg(long, default_value_t = 0)] since: i64 },
    Lease { #[command(subcommand)] cmd: LeaseCmd },
    /// Land a landable changeset (fast-forward, or Tower-built candidate; refuses red / non-landable)
    Land { changeset: String },
    /// MCP server over stdio (JSON-RPC 2.0, line-delimited)
    ServeMcp,
    Lsp,
    Duplicates,
    Github,
}

#[derive(Subcommand)]
enum TaskCmd {
    List,
    Create { title: String, #[arg(long, default_value = "")] brief: String },
    Show { id: String },
    Kill { id: String },
    PlanSave { id: String, #[arg(long)] plan: String },
}
#[derive(Subcommand)]
enum AssignmentCmd {
    List,
    Create { #[arg(long)] task: String, #[arg(long)] worktree: String, #[arg(long, default_value = "")] branch: String },
    Close { id: String },
}
#[derive(Subcommand)]
enum ClaimCmd {
    /// Nodes as repeated --node file or --node file#item; uncontested nodes are granted, the rest refused by name
    Acquire { #[arg(long)] assignment: String, #[arg(long = "node")] nodes: Vec<String> },
    /// With repeated --node, release only those nodes; without, release all of the assignment's claims
    Release { #[arg(long)] assignment: String, #[arg(long = "node")] nodes: Vec<String> },
    List,
}
#[derive(Subcommand)]
enum WorktreeCmd {
    Create { #[arg(long)] task: String },
    List,
}
#[derive(Subcommand)]
enum ChangesetCmd {
    Register { #[arg(long)] assignment: String, #[arg(long)] worktree: String },
    Status { id: String },
    List,
    /// Recompute live files/CVA/head and bump the changeset version
    Refresh { id: String },
    /// Manually supersede a changeset (excluded from order/statuses/radar)
    Supersede { id: String },
    /// Conflicting files + git merge-tree hunk headers vs main / earlier changesets (read-only)
    Conflicts { id: String, #[arg(long)] json: bool },
}
#[derive(Subcommand)]
enum GateCmd { Run { changeset: String, #[arg(long, default_value_t = 1)] tier: i64 } }
#[derive(Subcommand)]
enum EscalateCmd {
    Create { #[arg(long)] task: String, question: String, #[arg(long, default_value = "[]")] options: String, #[arg(long, default_value = "")] recommendation: String },
    Resolve { id: String, answer: String },
    List,
}
#[derive(Subcommand)]
enum DecisionsCmd { List, Log { text: String }, Revoke { id: String } }
#[derive(Subcommand)]
enum LeaseCmd { Acquire { #[arg(long)] holder: String }, Release { #[arg(long)] holder: String }, Status }

fn repo_root(cli_repo: &Option<PathBuf>) -> Result<PathBuf> {
    let start = cli_repo.clone().unwrap_or_else(|| std::env::current_dir().unwrap());
    git::repo_root(&start)
}
fn registered(store: &Store, cli_repo: &Option<PathBuf>) -> Result<(i64, PathBuf, TowerConfig)> {
    let root = repo_root(cli_repo)?;
    let canon = git::canonical_str(&root);
    let cfg = TowerConfig::load(&root);
    let repo = store.repo_by_path(&canon)?.with_context(|| format!("repo not registered: {canon} (run: tower init)"))?;
    Ok((repo.id, root, cfg))
}
fn parse_nodes(raw: &[String]) -> Vec<ClaimNode> {
    raw.iter().map(|n| match n.split_once('#') { Some((f, i)) => ClaimNode { file: f.to_string(), item: Some(i.to_string()) }, None => ClaimNode { file: n.clone(), item: None } }).collect()
}
fn print_json(v: &serde_json::Value) { println!("{}", serde_json::to_string_pretty(v).unwrap()); }

fn main() -> Result<()> {
    let cli = Cli::parse();
    let home = config::tower_home();
    let store = Store::open(home)?;
    run(&cli, &store)
}

fn run(cli: &Cli, store: &Store) -> Result<()> {
    match &cli.command {
        Command::Init { write_config, main_branch } => {
            let root = repo_root(&cli.repo)?;
            let canon = git::canonical_str(&root);
            let existing_cfg = TowerConfig::load(&root);
            let mb = main_branch.clone().unwrap_or_else(|| git::current_branch(&root).unwrap_or(existing_cfg.repo.main_branch.clone()));
            let cfg_path = root.join("tower.toml");
            let mut wrote = false;
            if !cfg_path.exists() {
                let approved = *write_config || ask_yes(&format!("Write default {}? [y/N] ", cfg_path.display()));
                if approved {
                    std::fs::write(&cfg_path, config::default_config_text(&mb, &root))?;
                    wrote = true;
                }
            }
            let repo = store.register_repo(&canon, &mb)?;
            store.emit(repo.id, "repo_registered", serde_json::json!({"repo": canon, "main_branch": mb, "config_written": wrote}))?;
            let cfg = TowerConfig::load(&root);
            print_json(&serde_json::json!({"repo": canon, "main_branch": mb, "config_written": wrote, "tiers": cfg.tier_label()}));
            Ok(())
        }
        Command::Board { html, json } => {
            let (rid, root, cfg) = registered(store, &cli.repo)?;
            let b = board::build_board(store, rid, &root, &cfg)?;
            if let Some(p) = html { std::fs::write(p, board::render_html(&b))?; }
            if *json { print_json(&b); } else { print!("{}", board::render_text(&b)); }
            Ok(())
        }
        Command::Watch { interval, once, iterations, run_tier1 } => cmd_watch(store, cli, *interval, *once, *iterations, *run_tier1),
        Command::Task { cmd } => {
            let (rid, _, _) = registered(store, &cli.repo)?;
            match cmd {
                TaskCmd::List => print_json(&serde_json::json!(store.list_tasks(rid)?)),
                TaskCmd::Create { title, brief } => { let t = store.create_task(rid, title, brief)?; print_json(&serde_json::json!({"task_id": t.id, "state": t.state})); }
                TaskCmd::Show { id } => { let t = store.get_task(rid, id)?.with_context(|| format!("unknown task {id}"))?; print_json(&serde_json::json!(t)); }
                TaskCmd::Kill { id } => {
                    for a in store.list_assignments(rid)?.into_iter().filter(|a| a.task_id == *id && a.state == "open") { store.close_assignment(rid, &a.id)?; }
                    store.set_task_state(rid, id, "failed")?;
                    print_json(&serde_json::json!({"task_id": id, "state": "failed"}));
                }
                TaskCmd::PlanSave { id, plan } => { store.save_plan(rid, id, plan)?; print_json(&serde_json::json!({"saved": true})); }
            }
            Ok(())
        }
        Command::Assignment { cmd } => {
            let (rid, root, _) = registered(store, &cli.repo)?;
            match cmd {
                AssignmentCmd::List => print_json(&serde_json::json!(store.list_assignments(rid)?)),
                AssignmentCmd::Create { task, worktree, branch } => {
                    let _ = root;
                    let b = if branch.is_empty() { git::current_branch(Path::new(worktree)).unwrap_or_default() } else { branch.clone() };
                    let a = store.create_assignment(rid, task, worktree, &b)?;
                    print_json(&serde_json::json!({"assignment_id": a.id, "task_id": a.task_id, "worktree": a.worktree_path, "branch": a.branch}));
                }
                AssignmentCmd::Close { id } => { store.close_assignment(rid, id)?; print_json(&serde_json::json!({"closed": true})); }
            }
            Ok(())
        }
        Command::Claim { cmd } => {
            let (rid, _, _) = registered(store, &cli.repo)?;
            match cmd {
                ClaimCmd::Acquire { assignment, nodes } => {
                    let parsed = parse_nodes(nodes);
                    let r = store.acquire_claims_partial(rid, assignment, &parsed)?;
                    let all = r.refused.is_empty();
                    print_json(&serde_json::json!({"granted": all, "granted_nodes": r.granted_nodes, "refused": r.refused.clone(), "collisions": r.refused}));
                    if r.granted_nodes.is_empty() && !parsed.is_empty() { bail!("claim refused: no nodes granted (see refused above)"); }
                }
                ClaimCmd::Release { assignment, nodes } => {
                    let parsed = parse_nodes(nodes);
                    store.release_claims_nodes(rid, assignment, if parsed.is_empty() { None } else { Some(&parsed) })?;
                    print_json(&serde_json::json!({"released": true, "nodes": parsed}));
                }
                ClaimCmd::List => print_json(&serde_json::json!(store.list_claims(rid)?)),
            }
            Ok(())
        }
        Command::Worktree { cmd } => {
            match cmd {
                WorktreeCmd::Create { task } => { let (rid, root, cfg) = registered(store, &cli.repo)?; print_json(&ops::worktree_create(store, rid, &root, &cfg, task)?); }
                WorktreeCmd::List => { let (_, root, _) = registered(store, &cli.repo)?; print_json(&serde_json::json!(git::worktree_list(&root)?)); }
            }
            Ok(())
        }
        Command::Changeset { cmd } => {
            let (rid, root, cfg) = registered(store, &cli.repo)?;
            match cmd {
                ChangesetCmd::Register { assignment, worktree } => print_json(&engine::register_changeset(store, rid, &root, &cfg, assignment, worktree)?),
                ChangesetCmd::Status { id } => print_json(&board::changeset_status_value(store, rid, &root, &cfg, id)?),
                ChangesetCmd::List => print_json(&serde_json::json!(store.list_changesets(rid)?)),
                ChangesetCmd::Refresh { id } => print_json(&engine::refresh_changeset(store, rid, &root, &cfg, id)?),
                ChangesetCmd::Supersede { id } => print_json(&engine::supersede_changeset(store, rid, &root, &cfg, id)?),
                ChangesetCmd::Conflicts { id, json } => {
                    let v = engine::changeset_conflicts(store, rid, &root, &cfg, id)?;
                    if *json { print_json(&v); } else { print!("{}", engine::render_conflicts_text(&v)); }
                }
            }
            Ok(())
        }
        Command::Gate { cmd } => {
            let (rid, root, cfg) = registered(store, &cli.repo)?;
            match cmd { GateCmd::Run { changeset, tier } => print_json(&engine::gate_run_single(store, rid, &root, &cfg, changeset, *tier)?) }
            Ok(())
        }
        Command::MergeCheck => {
            let (rid, root, cfg) = registered(store, &cli.repo)?;
            let tiers: Vec<i64> = [cfg.gates.check.is_some().then_some(1), cfg.gates.test.is_some().then_some(2)].into_iter().flatten().collect();
            let v = engine::rehearse(store, rid, &root, &cfg, &tiers)?;
            print_json(&v);
            let b = board::build_board(store, rid, &root, &cfg)?;
            print!("{}", board::render_text(&b));
            Ok(())
        }
        Command::Order => { let (rid, _, _) = registered(store, &cli.repo)?; print_json(&serde_json::json!({"order": engine::integration_order(store, rid)?})); Ok(()) }
        Command::Escalate { cmd } => {
            let (rid, _, _) = registered(store, &cli.repo)?;
            match cmd {
                EscalateCmd::Create { task, question, options, recommendation } => {
                    let opts: serde_json::Value = serde_json::from_str(options).unwrap_or(serde_json::json!([options]));
                    let e = store.create_escalation(rid, task, question, &opts, recommendation)?;
                    print_json(&serde_json::json!({"escalation_id": e.id}));
                }
                EscalateCmd::Resolve { id, answer } => { store.resolve_escalation(rid, id, answer)?; print_json(&serde_json::json!({"resolved": true})); }
                EscalateCmd::List => print_json(&serde_json::json!(store.list_escalations(rid)?)),
            }
            Ok(())
        }
        Command::Decisions { cmd } => {
            let (rid, _, _) = registered(store, &cli.repo)?;
            match cmd {
                DecisionsCmd::List => print_json(&serde_json::json!(store.list_decisions(rid)?)),
                DecisionsCmd::Log { text } => { let d = store.log_decision(rid, text)?; print_json(&serde_json::json!({"decision_id": d.id})); }
                DecisionsCmd::Revoke { id } => { store.revoke_decision(rid, id)?; print_json(&serde_json::json!({"revoked": true})); }
            }
            Ok(())
        }
        Command::Events { since } => {
            let (rid, _, _) = registered(store, &cli.repo)?;
            let ev = store.events_since(rid, *since)?;
            print_json(&serde_json::json!({"events": ev, "next_cursor": ev.last().map(|e| e.seq).unwrap_or(*since)}));
            Ok(())
        }
        Command::Lease { cmd } => {
            let (rid, _, _) = registered(store, &cli.repo)?;
            match cmd {
                LeaseCmd::Acquire { holder } => match store.lease_acquire(rid, holder)? {
                    Ok(()) => print_json(&serde_json::json!({"acquired": true})),
                    Err(h) => bail!("lease held by {h}"),
                },
                LeaseCmd::Release { holder } => { store.lease_release(rid, holder)?; print_json(&serde_json::json!({"released": true})); }
                LeaseCmd::Status => print_json(&serde_json::json!({"lease": store.lease_holder(rid)?.map(|(h,t)| serde_json::json!({"holder": h, "acquired_at": t}))})),
            }
            Ok(())
        }
        Command::Land { changeset } => {
            let (rid, root, cfg) = registered(store, &cli.repo)?;
            print_json(&engine::land_changeset(store, rid, &root, &cfg, changeset)?);
            Ok(())
        }
        Command::ServeMcp => mcp::serve(store),
        Command::Lsp | Command::Duplicates | Command::Github => { println!("not implemented in v1"); Ok(()) }
    }
}

fn ask_yes(prompt: &str) -> bool {
    if !std::io::stdin().is_terminal() { return false; }
    eprint!("{prompt}");
    let mut line = String::new();
    if std::io::stdin().read_line(&mut line).is_err() { return false; }
    matches!(line.trim(), "y" | "Y" | "yes" | "YES")
}

fn cmd_watch(store: &Store, cli: &Cli, interval: u64, once: bool, iterations: Option<usize>, run_tier1: bool) -> Result<()> {
    let (rid, root, cfg) = registered(store, &cli.repo)?;
    let total: Option<usize> = if once { Some(1) } else { iterations };
    eprintln!("tower watch: polling every {interval}s for {} (polling loop; no filesystem watcher in v1)", root.display());
    let html_path = store.home.join(config::repo_hash(&git::canonical_str(&root))).join("board.html");
    if let Some(p) = html_path.parent() { std::fs::create_dir_all(p)?; }
    let mut prev_collisions = String::new();
    let mut prev_diff = String::new();
    let mut n = 0usize;
    loop {
        let radar = engine::radar_scan(store, &root, &cfg)?;
        let cols_key = radar.collisions.iter().map(|c| format!("{}<->{}{}", c.a, c.b, c.files.join(","))).collect::<Vec<_>>().join(";");
        if cols_key != prev_collisions {
            for c in &radar.collisions {
                store.emit(rid, "collision_detected", serde_json::json!({"a": c.a, "b": c.b, "files": c.files}))?;
            }
            prev_collisions = cols_key;
        }
        let diff_key = radar.worktrees.iter().map(|w| format!("{}:{}:{}", w.path, w.snapshot, w.files.join(","))).collect::<Vec<_>>().join(";");
        if diff_key != prev_diff {
            if !prev_diff.is_empty() {
                store.emit(rid, "material_diff_changed", serde_json::json!({"worktrees": radar.worktrees.len()}))?;
                if run_tier1 && cfg.gates.check.is_some() {
                    let _ = engine::rehearse(store, rid, &root, &cfg, &[1]);
                }
            }
            prev_diff = diff_key;
            let _ = engine::refresh_changeset_statuses(store, rid, &root, &cfg);
        }
        let b = board::build_board(store, rid, &root, &cfg)?;
        std::fs::write(&html_path, board::render_html(&b))?;
        store.emit(rid, "watch_scan", serde_json::json!({"worktrees": radar.worktrees.len(), "collisions": radar.collisions.len()}))?;
        println!("scan: {} worktrees, {} collisions — board: {}", radar.worktrees.len(), radar.collisions.len(), html_path.display());
        for c in &radar.collisions {
            println!("  RED {} <-> {} in {}", engine::label_for(store, rid, &c.a), engine::label_for(store, rid, &c.b), c.files.join(","));
        }
        n += 1;
        if let Some(t) = total { if n >= t { break; } }
        std::thread::sleep(std::time::Duration::from_secs(interval.max(1)));
    }
    Ok(())
}
