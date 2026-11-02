//! Process-group cleanup shared by agent supervisors and test-resource owners.
//!
//! Callers must own an unreaped child that leads its dedicated process group.
//! Keeping the leader unreaped pins its PID/PGID until the last signal is sent.

use anyhow::{bail, Context, Result};
use std::process::{Child, ExitStatus};
use std::time::{Duration, Instant};

#[cfg(target_os = "macos")]
fn members(group: i32) -> Result<Vec<i32>> {
    // Apple SDK sys/proc_info.h: PROC_PGRP_ONLY=2. proc_listpids scans both
    // allproc and zombproc under the kernel's process-list lock. See:
    // https://github.com/apple-oss-distributions/xnu/blob/main/bsd/kern/proc_info.c
    const PROC_PGRP_ONLY: u32 = 2;
    let mut capacity = 256_usize;
    loop {
        let mut members = vec![0_i32; capacity];
        let byte_capacity = std::mem::size_of_val(members.as_slice());
        // libproc converts syscall failure to zero bytes, retaining errno. Clear
        // errno to distinguish a successfully empty group from denied inspection.
        unsafe {
            *libc::__error() = 0;
        }
        let bytes = unsafe {
            libc::proc_listpids(
                PROC_PGRP_ONLY,
                group as u32,
                members.as_mut_ptr().cast(),
                byte_capacity as i32,
            )
        };
        let error = std::io::Error::last_os_error();
        if bytes < 0 || (bytes == 0 && error.raw_os_error().is_some_and(|errno| errno != 0)) {
            return Err(error).context("enumerate owned process group");
        }
        if bytes as usize == byte_capacity {
            // A full buffer might omit a living member. Retry with more room;
            // never infer quiescence from a truncated process snapshot.
            if capacity >= 1_048_576 {
                bail!("owned process group enumeration exceeds inspection limit");
            }
            capacity *= 2;
            continue;
        }
        if bytes as usize > byte_capacity
            || !(bytes as usize).is_multiple_of(std::mem::size_of::<i32>())
        {
            bail!("invalid owned process group enumeration");
        }
        members.truncate(bytes as usize / std::mem::size_of::<i32>());
        return Ok(members);
    }
}

#[cfg(target_os = "macos")]
fn member_info(member: i32) -> Result<Option<libc::proc_bsdinfo>> {
    let mut info: libc::proc_bsdinfo = unsafe { std::mem::zeroed() };
    let expected = std::mem::size_of_val(&info) as i32;
    unsafe {
        *libc::__error() = 0;
    }
    // Nonzero arg explicitly includes zombies for BSD information. Start time
    // distinguishes a PID reused between membership and status snapshots.
    let read = unsafe {
        libc::proc_pidinfo(
            member,
            libc::PROC_PIDTBSDINFO,
            1,
            (&mut info as *mut libc::proc_bsdinfo).cast(),
            expected,
        )
    };
    if read != expected {
        let error = std::io::Error::last_os_error();
        if read == 0 && error.raw_os_error() == Some(libc::ESRCH) {
            return Ok(None);
        }
        bail!("cannot inspect owned process-group member {member}: {error}");
    }
    Ok(Some(info))
}

#[cfg(target_os = "macos")]
fn has_live_members(group: i32) -> Result<bool> {
    let mut stopped = std::collections::HashMap::new();
    for member in members(group)? {
        if let Some(info) = member_info(member)? {
            if info.pbi_pgid != group as u32 {
                return Ok(true);
            } // changing snapshot
            if info.pbi_status != libc::SZOMB {
                return Ok(true);
            }
            stopped.insert(member, (info.pbi_start_tvsec, info.pbi_start_tvusec));
        }
    }
    // A parent could fork after the first list, then exit before inspection.
    // Every member of a second complete snapshot must be an identity already
    // observed stopped. Such members cannot create an unseen new descendant.
    for member in members(group)? {
        if let Some(info) = member_info(member)? {
            if info.pbi_pgid != group as u32
                || info.pbi_status != libc::SZOMB
                || stopped.get(&member) != Some(&(info.pbi_start_tvsec, info.pbi_start_tvusec))
            {
                return Ok(true);
            }
        }
    }
    Ok(false)
}

#[cfg(unix)]
pub fn stop_owned_group(child: &mut Child) -> Result<ExitStatus> {
    let group = child.id() as i32;
    if unsafe { libc::kill(-group, libc::SIGKILL) } != 0 {
        let error = std::io::Error::last_os_error();
        let absent = error.raw_os_error() == Some(libc::ESRCH);
        #[cfg(target_os = "macos")]
        let zombie_only = error.raw_os_error() == Some(libc::EPERM) && !has_live_members(group)?;
        #[cfg(not(target_os = "macos"))]
        let zombie_only = false;
        // XNU killpg1 filters SZOMB members and returns EPERM for a group with
        // no signalable members. Accept that case only after OS enumeration has
        // proved that no living process can retain the protected resources.
        // https://github.com/apple-oss-distributions/xnu/blob/main/bsd/kern/kern_sig.c
        if !absent && !zombie_only {
            return Err(error).context("stop owned process group");
        }
    }
    #[cfg(target_os = "macos")]
    {
        let started = Instant::now();
        while has_live_members(group)? {
            if started.elapsed() >= Duration::from_secs(2) {
                bail!("owned process group still has living members; completion is uncertain");
            }
            std::thread::sleep(Duration::from_millis(25));
        }
        // Prove quiescence before reaping, while group identity is still pinned.
        child.wait().context("reap owned process-group leader")
    }
    #[cfg(not(target_os = "macos"))]
    {
        let status = child.wait().context("reap owned process-group leader")?;
        let started = Instant::now();
        loop {
            if unsafe { libc::kill(-group, 0) } != 0 {
                let error = std::io::Error::last_os_error();
                if error.raw_os_error() == Some(libc::ESRCH) {
                    return Ok(status);
                }
                return Err(error).context("confirm owned process group stopped");
            }
            if started.elapsed() >= Duration::from_secs(2) {
                bail!("owned process group still exists; completion is uncertain");
            }
            std::thread::sleep(Duration::from_millis(25));
        }
    }
}

#[cfg(all(test, target_os = "macos"))]
mod tests {
    use super::*;
    use std::os::unix::process::CommandExt;
    use std::process::{Command, Stdio};

    #[test]
    fn zombie_only_group_is_proven_quiescent_before_reaping() {
        let mut child = Command::new("/bin/sh")
            .args(["-c", "exit 7"])
            .stdin(Stdio::null())
            .stdout(Stdio::null())
            .stderr(Stdio::null())
            .process_group(0)
            .spawn()
            .unwrap();
        let mut info: libc::siginfo_t = unsafe { std::mem::zeroed() };
        assert_eq!(
            unsafe {
                libc::waitid(
                    libc::P_PID,
                    child.id() as libc::id_t,
                    &mut info,
                    libc::WEXITED | libc::WNOWAIT,
                )
            },
            0
        );
        assert!(!has_live_members(child.id() as i32).unwrap());
        assert_eq!(stop_owned_group(&mut child).unwrap().code(), Some(7));
    }

    #[test]
    fn live_group_is_never_mistaken_for_zombie_only() {
        let mut child = Command::new("/bin/sleep")
            .arg("30")
            .process_group(0)
            .spawn()
            .unwrap();
        assert!(has_live_members(child.id() as i32).unwrap());
        assert!(!stop_owned_group(&mut child).unwrap().success());
    }
}
