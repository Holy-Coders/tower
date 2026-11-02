//! Durable FIFO reservations for Tower scratch worktrees and shared test services.
//!
//! A running reservation never expires automatically. If its owner crashes, a
//! deliberate recovery is required after checking that its gate children stopped.
use anyhow::{bail, Context, Result};
use rusqlite::{params, OptionalExtension, Transaction, TransactionBehavior};
use serde::{Deserialize, Serialize};
use std::io::Read;
use std::path::Path;
use std::time::{Duration, Instant};

use crate::config::TowerConfig;
use crate::store::{now_secs, Store};

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Job {
    pub id: i64,
    pub repo_id: i64,
    pub request_key: String,
    pub resources: Vec<String>,
    pub details: serde_json::Value,
    pub state: String,
    pub owner_pid: Option<u32>,
    pub cancel_requested: bool,
    pub created_at: i64,
    pub started_at: Option<i64>,
    pub finished_at: Option<i64>,
}

pub fn migrate(store: &Store) -> Result<()> {
    store.conn.execute_batch(
        "CREATE TABLE IF NOT EXISTS resource_jobs(
          id INTEGER PRIMARY KEY, repo_id INTEGER NOT NULL, request_key TEXT NOT NULL,
          resources_json TEXT NOT NULL, details_json TEXT NOT NULL,
          state TEXT NOT NULL, owner_pid INTEGER, cancel_requested INTEGER NOT NULL DEFAULT 0,
          created_at INTEGER NOT NULL, started_at INTEGER, finished_at INTEGER);
         CREATE UNIQUE INDEX IF NOT EXISTS resource_jobs_active_key
          ON resource_jobs(repo_id,request_key) WHERE state IN ('queued','running');",
    )?;
    Ok(())
}

fn from_row(row: &rusqlite::Row<'_>) -> rusqlite::Result<Job> {
    let resources: String = row.get(3)?;
    let details: String = row.get(4)?;
    Ok(Job {
        id: row.get(0)?,
        repo_id: row.get(1)?,
        request_key: row.get(2)?,
        resources: serde_json::from_str(&resources).map_err(|e| {
            rusqlite::Error::FromSqlConversionFailure(3, rusqlite::types::Type::Text, Box::new(e))
        })?,
        details: serde_json::from_str(&details).map_err(|e| {
            rusqlite::Error::FromSqlConversionFailure(4, rusqlite::types::Type::Text, Box::new(e))
        })?,
        state: row.get(5)?,
        owner_pid: row.get(6)?,
        cancel_requested: row.get(7)?,
        created_at: row.get(8)?,
        started_at: row.get(9)?,
        finished_at: row.get(10)?,
    })
}

const COLUMNS: &str = "id,repo_id,request_key,resources_json,details_json,state,owner_pid,cancel_requested,created_at,started_at,finished_at";

pub fn list(store: &Store, repo_id: Option<i64>) -> Result<Vec<Job>> {
    let mut stmt = store.conn.prepare(&format!(
        "SELECT {COLUMNS} FROM resource_jobs WHERE (?1 IS NULL OR repo_id=?1) ORDER BY id"
    ))?;
    let rows = stmt.query_map([repo_id], from_row)?;
    Ok(rows.collect::<rusqlite::Result<Vec<_>>>()?)
}

fn get(store: &Store, repo_id: i64, id: i64) -> Result<Job> {
    store
        .conn
        .query_row(
            &format!("SELECT {COLUMNS} FROM resource_jobs WHERE repo_id=?1 AND id=?2"),
            params![repo_id, id],
            from_row,
        )
        .optional()?
        .with_context(|| format!("unknown resource job {id}"))
}

pub fn resource_names(repo_id: i64, cfg: &TowerConfig) -> Vec<String> {
    let mut resources = vec![format!("repo:{repo_id}:scratch")];
    resources.extend(
        cfg.gates
            .resources
            .iter()
            .map(|name| format!("shared:{name}")),
    );
    resources.sort();
    resources.dedup();
    resources
}

pub enum Reservation<'a> {
    Acquired(ResourceGuard<'a>),
    Queued(Job),
}

pub fn queued_receipt(job: &Job) -> serde_json::Value {
    serde_json::json!({"queued": true, "resource_job": job,
        "next": "retry the same operation after earlier reservations finish; resource cancel cancels a queued request"})
}

pub fn acquire<'a>(
    store: &'a Store,
    repo_id: i64,
    key: &str,
    resources: &[String],
    details: serde_json::Value,
) -> Result<Reservation<'a>> {
    if resources.is_empty() || resources.iter().any(|r| r.trim().is_empty()) {
        bail!("resource names must be nonempty");
    }
    // One writer transaction covers both FIFO selection and transition to running.
    let tx = Transaction::new_unchecked(&store.conn, TransactionBehavior::Immediate)?;
    let inserted = tx.execute("INSERT OR IGNORE INTO resource_jobs(repo_id,request_key,resources_json,details_json,state,created_at) VALUES(?1,?2,?3,?4,'queued',?5)",
        params![repo_id, key, serde_json::to_string(resources)?, details.to_string(), now_secs()])?;
    let job = tx.query_row(&format!("SELECT {COLUMNS} FROM resource_jobs WHERE repo_id=?1 AND request_key=?2 AND state IN ('queued','running')"), params![repo_id, key], from_row)?;
    if inserted > 0 {
        store.emit(
            repo_id,
            "resource_queued",
            serde_json::json!({"resource_job": job}),
        )?;
    }
    // Repeating an in-flight operation never grants its reservation twice.
    let mut blocked = job.state == "running";
    {
        let mut stmt = tx.prepare(&format!("SELECT {COLUMNS} FROM resource_jobs WHERE id<>?1 AND (state='running' OR (state='queued' AND id<?1)) ORDER BY id"))?;
        for other in stmt.query_map([job.id], from_row)? {
            let other = other?;
            if other.resources.iter().any(|r| job.resources.contains(r)) {
                blocked = true;
                break;
            }
        }
    }
    if blocked {
        tx.commit()?;
        return Ok(Reservation::Queued(job));
    }
    tx.execute(
        "UPDATE resource_jobs SET state='running',owner_pid=?1,started_at=?2 WHERE id=?3",
        params![std::process::id(), now_secs(), job.id],
    )?;
    store.emit(
        repo_id,
        "resource_acquired",
        serde_json::json!({"resource_job_id": job.id, "resources": job.resources}),
    )?;
    tx.commit()?;
    Ok(Reservation::Acquired(ResourceGuard {
        store,
        repo_id,
        id: job.id,
        release: true,
    }))
}

pub fn cancel(store: &Store, repo_id: i64, id: i64) -> Result<Job> {
    let tx = Transaction::new_unchecked(&store.conn, TransactionBehavior::Immediate)?;
    let job = get(store, repo_id, id)?;
    match job.state.as_str() {
        "queued" => {
            store.conn.execute("UPDATE resource_jobs SET state='cancelled',cancel_requested=1,finished_at=?1 WHERE id=?2 AND state='queued'", params![now_secs(), id])?;
        }
        "running" => {
            store.conn.execute(
                "UPDATE resource_jobs SET cancel_requested=1 WHERE id=?1 AND state='running'",
                [id],
            )?;
        }
        _ => {
            tx.commit()?;
            return Ok(job);
        }
    }
    store.emit(
        repo_id,
        "resource_cancel_requested",
        serde_json::json!({"resource_job_id": id}),
    )?;
    tx.commit()?;
    get(store, repo_id, id)
}

/// A dead owner is necessary but insufficient: a child may outlive it. Never
/// reclaim on a timer or PID liveness alone. The caller must verify children.
pub fn recover(store: &Store, repo_id: i64, id: i64, confirmed_stopped: bool) -> Result<Job> {
    let job = get(store, repo_id, id)?;
    if job.state != "running" {
        bail!("resource job {id} is not running");
    }
    if !confirmed_stopped {
        bail!("recovery requires --confirmed-stopped after verifying the gate process and children stopped");
    }
    if let Some(pid) = job.owner_pid {
        #[cfg(unix)]
        if unsafe { libc::kill(pid as i32, 0) } == 0
            || std::io::Error::last_os_error().raw_os_error() != Some(libc::ESRCH)
        {
            bail!("resource job {id} owner PID {pid} still exists; refusing recovery");
        }
    }
    store.conn.execute("UPDATE resource_jobs SET state='interrupted',finished_at=?1 WHERE id=?2 AND state='running'", params![now_secs(), id])?;
    store.emit(
        repo_id,
        "resource_recovered",
        serde_json::json!({"resource_job_id": id, "confirmed_stopped": true}),
    )?;
    get(store, repo_id, id)
}

pub struct ResourceGuard<'a> {
    store: &'a Store,
    repo_id: i64,
    pub id: i64,
    release: bool,
}

impl ResourceGuard<'_> {
    fn cancelled(&self) -> Result<bool> {
        Ok(get(self.store, self.repo_id, self.id)?.cancel_requested)
    }

    /// Gates stay in a process group whose leader remains unreaped while we
    /// terminate descendants. That pins the PID and prevents signalling a reused
    /// group ID. Output goes to Tower-owned files rather than blocking pipes.
    #[cfg(unix)]
    pub fn run_gate(&mut self, dir: &Path, cmd: &str, timeout_secs: u64) -> Result<(bool, String)> {
        use std::os::unix::process::CommandExt;
        use std::process::{Command, Stdio};
        if self.cancelled()? {
            return Ok((false, "gate cancelled before launch".into()));
        }
        if timeout_secs == 0 {
            bail!("gate timeout_secs must be greater than zero");
        }
        let logs = self.store.home.join("gate-logs");
        std::fs::create_dir_all(&logs)?;
        let out_path = logs.join(format!("{}.out", self.id));
        let err_path = logs.join(format!("{}.err", self.id));
        let output = std::fs::File::create(&out_path)?;
        let errors = std::fs::File::create(&err_path)?;
        let mut child = Command::new("sh")
            .arg("-c")
            .arg(cmd)
            .current_dir(dir)
            .stdin(Stdio::null())
            .stdout(output)
            .stderr(errors)
            .process_group(0)
            .spawn()?;
        let pid = child.id() as i32;
        let started = Instant::now();
        let mut reason = None;
        self.release = false; // Any observation/cleanup failure retains the reservation.
        loop {
            let mut info: libc::siginfo_t = unsafe { std::mem::zeroed() };
            let rc = unsafe {
                libc::waitid(
                    libc::P_PID,
                    pid as libc::id_t,
                    &mut info,
                    libc::WEXITED | libc::WNOHANG | libc::WNOWAIT,
                )
            };
            if rc != 0 {
                let e = std::io::Error::last_os_error();
                if e.kind() == std::io::ErrorKind::Interrupted {
                    continue;
                }
                return Err(e).context(
                    "gate observation failed; reservation retained for explicit recovery",
                );
            }
            if unsafe { info.si_pid() } != 0 {
                break;
            }
            if self.cancelled()? {
                reason = Some("gate cancelled");
                break;
            }
            if started.elapsed() >= Duration::from_secs(timeout_secs) {
                reason = Some("gate timed out");
                break;
            }
            std::thread::sleep(Duration::from_millis(25));
        }
        // The shared cleanup helper keeps the leader unreaped while proving
        // quiescence, including macOS zombie-only process groups.
        let status = crate::process::stop_owned_group(&mut child).with_context(|| {
            format!(
                "gate cleanup failed; resource job {} retained for explicit recovery",
                self.id
            )
        })?;
        self.release = true;
        let mut bytes = Vec::new();
        std::fs::File::open(out_path)?
            .take(8000)
            .read_to_end(&mut bytes)?;
        let remaining = 8000_usize.saturating_sub(bytes.len());
        std::fs::File::open(err_path)?
            .take(remaining as u64)
            .read_to_end(&mut bytes)?;
        let mut text = String::from_utf8_lossy(&bytes).into_owned();
        if let Some(reason) = reason {
            text.push_str(&format!("\n{reason}"));
        }
        Ok((status.success() && reason.is_none(), text))
    }

    #[cfg(not(unix))]
    pub fn run_gate(
        &mut self,
        _dir: &Path,
        _cmd: &str,
        _timeout_secs: u64,
    ) -> Result<(bool, String)> {
        bail!("bounded gate execution requires Unix process groups")
    }
}

impl Drop for ResourceGuard<'_> {
    fn drop(&mut self) {
        if !self.release {
            return;
        }
        let state = if self.cancelled().unwrap_or(false) {
            "cancelled"
        } else {
            "completed"
        };
        if self
            .store
            .conn
            .execute(
                "UPDATE resource_jobs SET state=?1,finished_at=?2 WHERE id=?3 AND state='running'",
                params![state, now_secs(), self.id],
            )
            .is_ok()
        {
            let _ = self.store.emit(
                self.repo_id,
                "resource_released",
                serde_json::json!({"resource_job_id": self.id, "state": state}),
            );
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn reserve<'a>(store: &'a Store, repo: i64, key: &str, names: &[&str]) -> Reservation<'a> {
        acquire(
            store,
            repo,
            key,
            &names.iter().map(|s| s.to_string()).collect::<Vec<_>>(),
            serde_json::json!({"operation":"test"}),
        )
        .unwrap()
    }

    #[test]
    fn fifo_dedupe_cancel_and_independent_resources() {
        let store = Store::open_memory().unwrap();
        migrate(&store).unwrap();
        let Reservation::Acquired(first) = reserve(&store, 1, "first", &["database"]) else {
            panic!()
        };
        assert!(matches!(
            reserve(&store, 1, "first", &["database"]),
            Reservation::Queued(_)
        ));
        let Reservation::Queued(second) = reserve(&store, 2, "second", &["database"]) else {
            panic!()
        };
        assert!(matches!(
            reserve(&store, 3, "third", &["database"]),
            Reservation::Queued(_)
        ));
        assert!(matches!(
            reserve(&store, 1, "independent", &["network"]),
            Reservation::Acquired(_)
        ));
        assert_eq!(list(&store, None).unwrap().len(), 4);
        drop(first);
        assert!(matches!(
            reserve(&store, 3, "third", &["database"]),
            Reservation::Queued(_)
        ));
        cancel(&store, 2, second.id).unwrap();
        assert!(matches!(
            reserve(&store, 3, "third", &["database"]),
            Reservation::Acquired(_)
        ));
    }

    #[test]
    fn running_is_not_reclaimed_and_recovery_is_deliberate() {
        let store = Store::open_memory().unwrap();
        migrate(&store).unwrap();
        let Reservation::Acquired(first) = reserve(&store, 1, "first", &["database"]) else {
            panic!()
        };
        let id = first.id;
        std::mem::forget(first);
        assert!(recover(&store, 1, id, false).is_err());
        assert!(recover(&store, 1, id, true).is_err()); // Live owner cannot be recovered.
        store
            .conn
            .execute(
                "UPDATE resource_jobs SET owner_pid=2147483647 WHERE id=?1",
                [id],
            )
            .unwrap();
        assert_eq!(recover(&store, 1, id, true).unwrap().state, "interrupted");
        assert!(matches!(
            reserve(&store, 1, "first", &["database"]),
            Reservation::Acquired(_)
        ));
    }

    #[test]
    fn separate_connections_cannot_acquire_same_resource() {
        let temp = tempfile::tempdir().unwrap();
        let one = Store::open(temp.path().into()).unwrap();
        migrate(&one).unwrap();
        let two = Store::open(temp.path().into()).unwrap();
        migrate(&two).unwrap();
        let Reservation::Acquired(guard) = reserve(&one, 1, "one", &["service"]) else {
            panic!()
        };
        assert!(matches!(
            reserve(&two, 2, "two", &["service"]),
            Reservation::Queued(_)
        ));
        drop(guard);
        assert!(matches!(
            reserve(&two, 2, "two", &["service"]),
            Reservation::Acquired(_)
        ));
    }

    #[test]
    fn concurrent_connections_have_exactly_one_winner() {
        use std::sync::{Arc, Barrier};
        let temp = tempfile::tempdir().unwrap();
        let seed = Store::open(temp.path().into()).unwrap();
        migrate(&seed).unwrap();
        drop(seed);
        let start = Arc::new(Barrier::new(2));
        let finish = Arc::new(Barrier::new(2));
        let handles: Vec<_> = (1..=2)
            .map(|repo| {
                let path = temp.path().to_path_buf();
                let start = start.clone();
                let finish = finish.clone();
                std::thread::spawn(move || {
                    let store = Store::open(path).unwrap();
                    start.wait();
                    let reservation = reserve(&store, repo, "race", &["shared"]);
                    let won = matches!(&reservation, Reservation::Acquired(_));
                    finish.wait();
                    drop(reservation);
                    won
                })
            })
            .collect();
        let winners = handles
            .into_iter()
            .map(|h| h.join().unwrap())
            .filter(|won| *won)
            .count();
        assert_eq!(winners, 1);
    }

    #[cfg(unix)]
    #[test]
    fn completed_gates_preserve_exit_status_and_release() {
        let temp = tempfile::tempdir().unwrap();
        let store = Store::open(temp.path().into()).unwrap();
        migrate(&store).unwrap();
        for (key, command, expected) in
            [("pass", "printf 'héllo'", true), ("fail", "exit 7", false)]
        {
            let Reservation::Acquired(mut guard) = reserve(&store, 1, key, &["service"]) else {
                panic!()
            };
            let (passed, _) = guard.run_gate(temp.path(), command, 1).unwrap();
            assert_eq!(passed, expected);
            let id = guard.id;
            drop(guard);
            assert_eq!(get(&store, 1, id).unwrap().state, "completed");
        }
    }

    #[cfg(unix)]
    #[test]
    fn gates_capture_unicode_bound_timeout_and_respect_cancellation() {
        let temp = tempfile::tempdir().unwrap();
        let store = Store::open(temp.path().into()).unwrap();
        migrate(&store).unwrap();
        let Reservation::Acquired(mut guard) = reserve(&store, 1, "timeout", &["service"]) else {
            panic!()
        };
        let (passed, output) = guard
            .run_gate(temp.path(), "printf 'héllo'; sleep 5", 1)
            .unwrap();
        assert!(!passed);
        assert!(output.contains("héllo"));
        assert!(output.contains("timed out"));
        let id = guard.id;
        drop(guard);
        assert_eq!(get(&store, 1, id).unwrap().state, "completed");
        let Reservation::Acquired(mut guard) = reserve(&store, 1, "cancel", &["service"]) else {
            panic!()
        };
        cancel(&store, 1, guard.id).unwrap();
        let (passed, _) = guard.run_gate(temp.path(), "exit 0", 1).unwrap();
        assert!(!passed);
        let id = guard.id;
        drop(guard);
        assert_eq!(get(&store, 1, id).unwrap().state, "cancelled");
    }
}
