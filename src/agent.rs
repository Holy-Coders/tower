//! Durable local agent adapter. Only an explicitly configured executable is run.
//!
//! A run directory is a permanent idempotency key. A separate Tower supervisor
//! owns the child, so restarting the coordinator never requires relaunching it.
//! Missing/stale receipts are uncertain, rather than permission to start twice.

use anyhow::{bail, Context, Result};
use serde::{Deserialize, Serialize};
use std::fs::{File, OpenOptions};
use std::io::Write;
use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};
use std::time::{Duration, Instant};

const HEARTBEAT_GRACE_SECS: i64 = 10;

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct AgentSpec {
    pub program: String,
    #[serde(default)]
    pub args: Vec<String>,
    pub timeout_secs: u64,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(tag = "state", rename_all = "snake_case")]
pub enum AgentStatus {
    Starting,
    Running,
    Cancelling,
    Succeeded {
        exit_code: i32,
    },
    Failed {
        exit_code: Option<i32>,
        message: String,
    },
    Cancelled,
    TimedOut,
    Uncertain {
        reason: String,
    },
}

impl AgentStatus {
    pub fn state(&self) -> &'static str {
        match self {
            Self::Starting => "starting",
            Self::Running => "running",
            Self::Cancelling => "cancelling",
            Self::Succeeded { .. } => "succeeded",
            Self::Failed { .. } => "failed",
            Self::Cancelled => "cancelled",
            Self::TimedOut => "timed_out",
            Self::Uncertain { .. } => "uncertain",
        }
    }

    pub fn is_terminal(&self) -> bool {
        matches!(
            self,
            Self::Succeeded { .. } | Self::Failed { .. } | Self::Cancelled | Self::TimedOut
        )
    }

    pub fn retryable(&self) -> bool {
        matches!(self, Self::Failed { .. } | Self::TimedOut)
    }
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
struct Request {
    worktree: PathBuf,
    spec: AgentSpec,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
struct Receipt {
    status: AgentStatus,
    updated_at: i64,
}

fn private_file(path: &Path) -> Result<File> {
    let mut options = OpenOptions::new();
    options.create_new(true).write(true);
    #[cfg(unix)]
    {
        use std::os::unix::fs::OpenOptionsExt;
        options.mode(0o600);
    }
    options
        .open(path)
        .with_context(|| format!("create {}", path.display()))
}

fn write_json(path: &Path, value: &impl Serialize) -> Result<()> {
    // There is one writer per receipt: only the winning launcher writes the
    // starting receipt; only the winning supervisor writes subsequent receipts.
    let temporary = path.with_extension("tmp");
    let mut options = OpenOptions::new();
    options.create(true).truncate(true).write(true);
    #[cfg(unix)]
    {
        use std::os::unix::fs::OpenOptionsExt;
        options.mode(0o600);
    }
    let mut file = options.open(&temporary)?;
    serde_json::to_writer(&mut file, value)?;
    file.write_all(b"\n")?;
    file.sync_all()?;
    std::fs::rename(temporary, path)?;
    Ok(())
}

fn receipt(run_dir: &Path, status: AgentStatus, final_receipt: bool) -> Result<()> {
    write_json(
        &run_dir.join(if final_receipt {
            "result.json"
        } else {
            "heartbeat.json"
        }),
        &Receipt {
            status,
            updated_at: crate::store::now_secs(),
        },
    )
}

fn request(run_dir: &Path) -> Result<Request> {
    Ok(serde_json::from_slice(&std::fs::read(
        run_dir.join("request.json"),
    )?)?)
}

/// Start once, or observe the same immutable run. The executable receives the
/// prompt on stdin and executes directly in the assignment worktree.
pub fn launch(
    run_dir: &Path,
    worktree: &Path,
    spec: &AgentSpec,
    prompt: &str,
) -> Result<AgentStatus> {
    launch_with_runner(run_dir, worktree, spec, prompt, &std::env::current_exe()?)
}

fn launch_with_runner(
    run_dir: &Path,
    worktree: &Path,
    spec: &AgentSpec,
    prompt: &str,
    runner: &Path,
) -> Result<AgentStatus> {
    if spec.program.trim().is_empty() || spec.timeout_secs == 0 {
        bail!("agent program and positive timeout_secs are required");
    }
    let worktree = std::fs::canonicalize(worktree).context("resolve agent worktree")?;
    if !worktree.is_dir() {
        bail!("agent worktree is not a directory");
    }
    let expected = Request {
        worktree,
        spec: spec.clone(),
    };
    if let Some(parent) = run_dir.parent() {
        std::fs::create_dir_all(parent)?;
    }
    match std::fs::create_dir(run_dir) {
        Ok(()) => {}
        Err(error) if error.kind() == std::io::ErrorKind::AlreadyExists => {
            // Never interpret a partially written launch as permission to retry.
            if let Ok(existing) = request(run_dir) {
                if existing != expected
                    || std::fs::read_to_string(run_dir.join("prompt.txt"))
                        .ok()
                        .as_deref()
                        != Some(prompt)
                {
                    bail!("agent run directory already belongs to a different request");
                }
            }
            return observe(run_dir);
        }
        Err(error) => return Err(error.into()),
    }
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        std::fs::set_permissions(run_dir, std::fs::Permissions::from_mode(0o700))?;
    }
    // The atomic directory above is the launch fence. Errors/crashes below can
    // leave an uncertain run, which is deliberately never launched a second time.
    write_json(&run_dir.join("request.json"), &expected)?;
    private_file(&run_dir.join("prompt.txt"))?.write_all(prompt.as_bytes())?;
    receipt(run_dir, AgentStatus::Starting, false)?;
    let run_dir = std::fs::canonicalize(run_dir)?;
    let log = private_file(&run_dir.join("supervisor.log"))?;
    let mut command = Command::new(runner);
    command
        .args(["__agent-runner", "--run-dir"])
        .arg(&run_dir)
        .stdin(Stdio::null())
        .stdout(Stdio::null())
        .stderr(log);
    #[cfg(unix)]
    {
        use std::os::unix::process::CommandExt;
        command.process_group(0);
    }
    match command.spawn() {
        Ok(mut supervisor) => {
            // Reap in a live coordinator without making launch block on the
            // agent. If the coordinator exits, the OS adopts the supervisor.
            std::thread::spawn(move || {
                let _ = supervisor.wait();
            });
            observe(&run_dir)
        }
        Err(error) => {
            let status = AgentStatus::Failed {
                exit_code: None,
                message: format!("start agent supervisor: {error}"),
            };
            receipt(&run_dir, status.clone(), true)?;
            Ok(status)
        }
    }
}

/// Read durable completion or recent liveness. No stored PID is ever signalled.
pub fn observe(run_dir: &Path) -> Result<AgentStatus> {
    let result = run_dir.join("result.json");
    if result.exists() {
        let stored: Receipt = serde_json::from_slice(&std::fs::read(result)?)?;
        return Ok(stored.status);
    }
    let heartbeat = run_dir.join("heartbeat.json");
    let stored: Receipt = match std::fs::read(heartbeat)
        .ok()
        .and_then(|data| serde_json::from_slice(&data).ok())
    {
        Some(value) => value,
        None => {
            return Ok(AgentStatus::Uncertain {
                reason: "agent launch has no complete receipt; do not relaunch this attempt".into(),
            })
        }
    };
    if crate::store::now_secs().saturating_sub(stored.updated_at) > HEARTBEAT_GRACE_SECS {
        return Ok(AgentStatus::Uncertain { reason: "agent supervisor heartbeat is stale; reserve ownership and investigate the run directory".into() });
    }
    if run_dir.join("cancel").exists() {
        return Ok(AgentStatus::Cancelling);
    }
    Ok(stored.status)
}

/// Prove that this immutable request targeted the expected worktree and has a
/// definitive stopped receipt. Missing/corrupt/uncertain state fails closed.
pub fn quiescent(run_dir: &Path, expected_worktree: &Path) -> Result<bool> {
    let expected = std::fs::canonicalize(expected_worktree)?;
    let Ok(stored) = request(run_dir) else {
        return Ok(false);
    };
    if stored.worktree != expected {
        return Ok(false);
    }
    Ok(observe(run_dir)?.is_terminal())
}

/// Request cancellation through the owning supervisor. Uncertain runs stay
/// uncertain until that supervisor confirms the process has stopped.
pub fn cancel(run_dir: &Path) -> Result<AgentStatus> {
    if !run_dir.is_dir() {
        bail!("unknown agent run directory {}", run_dir.display());
    }
    let status = observe(run_dir)?;
    if status.is_terminal() {
        return Ok(status);
    }
    match private_file(&run_dir.join("cancel")) {
        Ok(mut file) => {
            file.write_all(b"cancel requested\n")?;
        }
        Err(error) if run_dir.join("cancel").exists() => {
            let _ = error;
        }
        Err(error) => return Err(error),
    }
    observe(run_dir)
}

/// Fence a reservation that the scheduler knows has never reached launch.
/// The caller must serialize this with launch using its coordination lock.
pub fn cancel_unlaunched(run_dir: &Path) -> Result<AgentStatus> {
    if let Some(parent) = run_dir.parent() {
        std::fs::create_dir_all(parent)?;
    }
    match std::fs::create_dir(run_dir) {
        Ok(()) => {
            private_file(&run_dir.join("cancel"))?.write_all(b"cancelled before launch\n")?;
            receipt(run_dir, AgentStatus::Cancelled, true)?;
            Ok(AgentStatus::Cancelled)
        }
        Err(error) if error.kind() == std::io::ErrorKind::AlreadyExists => cancel(run_dir),
        Err(error) => Err(error.into()),
    }
}

#[cfg(unix)]
fn stop_child(child: &mut std::process::Child) -> Result<std::process::ExitStatus> {
    crate::process::stop_owned_group(child)
}

#[cfg(unix)]
fn exited_without_reaping(child: &std::process::Child) -> Result<bool> {
    let mut info: libc::siginfo_t = unsafe { std::mem::zeroed() };
    let result = unsafe {
        libc::waitid(
            libc::P_PID,
            child.id() as libc::id_t,
            &mut info,
            libc::WEXITED | libc::WNOHANG | libc::WNOWAIT,
        )
    };
    if result != 0 {
        return Err(std::io::Error::last_os_error()).context("observe owned agent without reaping");
    }
    Ok(unsafe { info.si_pid() } != 0)
}

#[cfg(not(unix))]
fn stop_child(child: &mut std::process::Child) -> Result<std::process::ExitStatus> {
    child.kill()?;
    Ok(child.wait()?)
}

/// Hidden CLI entry point. Dispatch this before opening Tower's store: the
/// supervisor is independent of database migrations and coordinator leases.
pub fn run_supervisor(run_dir: &Path) -> Result<()> {
    match std::fs::create_dir(run_dir.join("supervisor.lock")) {
        Ok(()) => {}
        Err(error) if error.kind() == std::io::ErrorKind::AlreadyExists => return Ok(()),
        Err(error) => return Err(error.into()),
    }
    let request = request(run_dir)?;
    if run_dir.join("result.json").exists() {
        return Ok(());
    }
    if run_dir.join("cancel").exists() {
        return receipt(run_dir, AgentStatus::Cancelled, true);
    }
    let stdout = private_file(&run_dir.join("stdout.log"))?;
    let stderr = private_file(&run_dir.join("stderr.log"))?;
    let stdin = File::open(run_dir.join("prompt.txt"))?;
    let mut command = Command::new(&request.spec.program);
    command
        .args(&request.spec.args)
        .current_dir(&request.worktree)
        .stdin(stdin)
        .stdout(stdout)
        .stderr(stderr)
        .env("TOWER_RUN_DIR", run_dir);
    #[cfg(unix)]
    {
        use std::os::unix::process::CommandExt;
        command.process_group(0);
    }
    let mut child = match command.spawn() {
        Ok(child) => child,
        Err(error) => {
            return receipt(
                run_dir,
                AgentStatus::Failed {
                    exit_code: None,
                    message: format!("start configured agent: {error}"),
                },
                true,
            )
        }
    };
    let started = Instant::now();
    let mut heartbeat = Instant::now() - Duration::from_secs(1);
    loop {
        // Check cancellation/timeout before try_wait() reaps a finished child;
        // stop_child's process-group ownership argument depends on that order.
        let stopped = if run_dir.join("cancel").exists() {
            Some(AgentStatus::Cancelled)
        } else if started.elapsed() >= Duration::from_secs(request.spec.timeout_secs) {
            Some(AgentStatus::TimedOut)
        } else {
            None
        };
        if let Some(status) = stopped {
            stop_child(&mut child)?;
            return receipt(run_dir, status, true);
        }
        #[cfg(unix)]
        let completed = if exited_without_reaping(&child)? {
            Some(stop_child(&mut child)?)
        } else {
            None
        };
        #[cfg(not(unix))]
        let completed = child.try_wait()?;
        if let Some(exit) = completed {
            let status = if exit.success() {
                AgentStatus::Succeeded {
                    exit_code: exit.code().unwrap_or(0),
                }
            } else {
                AgentStatus::Failed {
                    exit_code: exit.code(),
                    message: "configured agent exited unsuccessfully; inspect stderr.log".into(),
                }
            };
            return receipt(run_dir, status, true);
        }
        if heartbeat.elapsed() >= Duration::from_secs(1) {
            receipt(run_dir, AgentStatus::Running, false)?;
            heartbeat = Instant::now();
        }
        std::thread::sleep(Duration::from_millis(50));
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn prepared(root: &Path, script: &str, timeout_secs: u64) -> PathBuf {
        let run = root.join("run");
        std::fs::create_dir(&run).unwrap();
        let spec = AgentSpec {
            program: "/bin/sh".into(),
            args: vec!["-c".into(), script.into()],
            timeout_secs,
        };
        write_json(
            &run.join("request.json"),
            &Request {
                worktree: std::fs::canonicalize(root).unwrap(),
                spec,
            },
        )
        .unwrap();
        private_file(&run.join("prompt.txt"))
            .unwrap()
            .write_all(b"fixture prompt\n")
            .unwrap();
        receipt(&run, AgentStatus::Starting, false).unwrap();
        run
    }

    fn wait_running(run: &Path) {
        for _ in 0..100 {
            if matches!(observe(run).unwrap(), AgentStatus::Running) {
                return;
            }
            std::thread::sleep(Duration::from_millis(10));
        }
        panic!("fixture agent did not start");
    }

    #[test]
    fn supervisor_records_success_and_prompt_without_opening_store() {
        let temporary = tempfile::tempdir().unwrap();
        let run = prepared(temporary.path(), "cat; printf 'done'", 5);
        run_supervisor(&run).unwrap();
        assert_eq!(
            observe(&run).unwrap(),
            AgentStatus::Succeeded { exit_code: 0 }
        );
        assert_eq!(
            std::fs::read_to_string(run.join("stdout.log")).unwrap(),
            "fixture prompt\ndone"
        );
        assert!(!temporary.path().join("tower.db").exists());
        run_supervisor(&run).unwrap(); // a duplicate supervisor cannot run again
        assert_eq!(
            std::fs::read_to_string(run.join("stdout.log")).unwrap(),
            "fixture prompt\ndone"
        );
    }

    #[test]
    fn failure_receipt_is_retryable_but_uncertain_launch_is_not() {
        let temporary = tempfile::tempdir().unwrap();
        let run = prepared(temporary.path(), "exit 7", 5);
        run_supervisor(&run).unwrap();
        let state = observe(&run).unwrap();
        assert!(matches!(
            state,
            AgentStatus::Failed {
                exit_code: Some(7),
                ..
            }
        ));
        assert!(state.retryable());
        std::fs::remove_file(run.join("result.json")).unwrap();
        write_json(
            &run.join("heartbeat.json"),
            &Receipt {
                status: AgentStatus::Running,
                updated_at: 0,
            },
        )
        .unwrap();
        let state = observe(&run).unwrap();
        assert!(matches!(state, AgentStatus::Uncertain { .. }));
        assert!(!state.retryable());
        assert!(!state.is_terminal());
    }

    #[test]
    fn cancel_is_confirmed_by_owning_supervisor() {
        let temporary = tempfile::tempdir().unwrap();
        let run = prepared(temporary.path(), "sleep 30", 60);
        std::thread::scope(|scope| {
            let worker = scope.spawn(|| run_supervisor(&run));
            wait_running(&run);
            assert_eq!(cancel(&run).unwrap(), AgentStatus::Cancelling);
            worker.join().unwrap().unwrap();
        });
        assert_eq!(observe(&run).unwrap(), AgentStatus::Cancelled);
        assert_eq!(cancel(&run).unwrap(), AgentStatus::Cancelled);
    }

    #[test]
    fn timeout_is_bounded_and_duplicate_launch_does_not_respawn() {
        let temporary = tempfile::tempdir().unwrap();
        let run = prepared(temporary.path(), "sleep 30", 1);
        let started = Instant::now();
        run_supervisor(&run).unwrap();
        assert!(started.elapsed() < Duration::from_secs(5));
        assert_eq!(observe(&run).unwrap(), AgentStatus::TimedOut);
        let stored = request(&run).unwrap();
        assert_eq!(
            launch(&run, temporary.path(), &stored.spec, "fixture prompt\n").unwrap(),
            AgentStatus::TimedOut
        );
    }

    #[test]
    fn incomplete_existing_directory_fences_launch_and_cancel_does_not_signal_pid() {
        let temporary = tempfile::tempdir().unwrap();
        let run = temporary.path().join("incomplete");
        std::fs::create_dir(&run).unwrap();
        let spec = AgentSpec {
            program: "/nonexistent-fixture-agent".into(),
            args: vec![],
            timeout_secs: 1,
        };
        assert!(matches!(
            launch(&run, temporary.path(), &spec, "fixture").unwrap(),
            AgentStatus::Uncertain { .. }
        ));
        assert!(matches!(
            cancel(&run).unwrap(),
            AgentStatus::Uncertain { .. }
        ));
        assert!(run.join("cancel").exists());
        assert!(!run.join("request.json").exists());
    }

    #[test]
    fn known_supervisor_spawn_failure_is_terminal_and_different_request_is_refused() {
        let temporary = tempfile::tempdir().unwrap();
        let run = temporary.path().join("run");
        let spec = AgentSpec {
            program: "/fixture-agent".into(),
            args: vec![],
            timeout_secs: 1,
        };
        let state = launch_with_runner(
            &run,
            temporary.path(),
            &spec,
            "fixture",
            Path::new("/nonexistent-supervisor"),
        )
        .unwrap();
        assert!(matches!(
            state,
            AgentStatus::Failed {
                exit_code: None,
                ..
            }
        ));
        assert!(launch(&run, temporary.path(), &spec, "different prompt").is_err());
    }

    #[test]
    fn concurrent_supervisors_only_execute_once() {
        let temporary = tempfile::tempdir().unwrap();
        let run = prepared(
            temporary.path(),
            "printf 'once\\n' >> executions; sleep 0.1",
            5,
        );
        std::thread::scope(|scope| {
            let first = scope.spawn(|| run_supervisor(&run));
            let second = scope.spawn(|| run_supervisor(&run));
            first.join().unwrap().unwrap();
            second.join().unwrap().unwrap();
        });
        assert_eq!(
            std::fs::read_to_string(temporary.path().join("executions")).unwrap(),
            "once\n"
        );
        assert_eq!(
            observe(&run).unwrap(),
            AgentStatus::Succeeded { exit_code: 0 }
        );
    }

    #[cfg(unix)]
    #[test]
    fn terminal_receipt_stops_background_descendants_before_releasing_ownership() {
        let temporary = tempfile::tempdir().unwrap();
        let run = prepared(
            temporary.path(),
            "(sleep 0.4; printf escaped > escaped) & exit 7",
            5,
        );
        run_supervisor(&run).unwrap();
        assert!(matches!(
            observe(&run).unwrap(),
            AgentStatus::Failed {
                exit_code: Some(7),
                ..
            }
        ));
        assert!(quiescent(&run, temporary.path()).unwrap());
        std::thread::sleep(Duration::from_millis(600));
        assert!(!temporary.path().join("escaped").exists());
        assert!(!quiescent(&run, &temporary.path().join("missing")).unwrap_or(false));
    }
}
