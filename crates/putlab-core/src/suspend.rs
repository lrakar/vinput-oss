//! Stops a helper process without ending it, and starts it again.
//!
//! A suspended process keeps everything it has loaded and does no work at
//! all: none of its threads is scheduled, so it wakes the processor not once.
//! That is what a loaded speech model should cost between dictations.
//!
//! Windows has three ways to do this. The one used here is the documented
//! one: `SuspendThread` on every thread of the process, found through a
//! toolhelp snapshot, with the thread handles kept so that resuming is exact
//! and needs no second search.
//!
//! - `NtSuspendProcess` and `NtResumeProcess` do it in one call each, but they
//!   are exports of `ntdll.dll` that Microsoft does not document or promise to
//!   keep, and they have no import in `windows-sys`.
//! - Freezing the job the helper is in (`JobObjectFreezeInformation`) is what
//!   Windows does to store apps. Neither the information class nor its
//!   structure is in the SDK.
//!
//! What the one-call ways have over this one is that they cannot miss a
//! thread. Here that is made up for by looking again until a snapshot shows no
//! thread of the process that is not already held, and by giving up, with
//! everything resumed, if that does not happen. A thread that some other
//! program starts inside the helper afterwards would run; it could cost a
//! little work, never a wrong result, because resuming does not depend on it.
//!
//! Suspending is safe for the helper because Vinput shares no lock with it
//! and all its threads stop and start together: a thread stopped while holding
//! a lock holds it for nobody. A suspended process can be ended like any
//! other, by `TerminateProcess` and by the job it is in (see [`crate::job`]).
#![allow(unsafe_code)]

use std::io;
use std::process::Child;

/// The threads of a suspended process. The process runs again when this is
/// resumed or dropped, so losing track of one cannot leave a process stopped.
#[derive(Debug)]
pub struct Suspended(platform::Threads);

/// Suspends every thread of `child`.
///
/// # Errors
///
/// If the threads cannot all be suspended. The process is then left running
/// as it was.
pub fn suspend(child: &Child) -> io::Result<Suspended> {
    platform::suspend(child).map(Suspended)
}

impl Suspended {
    /// Lets the process run again. Its threads can be scheduled by the time
    /// this returns.
    ///
    /// # Errors
    ///
    /// If a thread could not be resumed, or is still held suspended by some
    /// other program. Every other thread has been resumed all the same.
    pub fn resume(mut self) -> io::Result<()> {
        self.0.resume()
    }
}

/// Whether the process with this identifier has a TCP connection open: one it
/// accepted or made, as opposed to a socket it only listens on. `None` if
/// Windows does not say. A server that answers each request on a connection
/// of its own is in the middle of one exactly when this is true, and must
/// not be suspended then.
#[must_use]
pub fn has_connection(process: u32) -> Option<bool> {
    platform::has_connection(process)
}

/// The processor time `child` has used since it started, in user and kernel
/// mode together, for tests to tell a process at work from one that is not.
/// `None` if Windows does not say.
#[cfg(windows)]
#[must_use]
pub fn cpu_time(child: &Child) -> Option<std::time::Duration> {
    use std::os::windows::io::AsRawHandle;

    use windows_sys::Win32::Foundation::FILETIME;
    use windows_sys::Win32::System::Threading::GetProcessTimes;

    let mut created = FILETIME::default();
    let mut exited = FILETIME::default();
    let mut kernel = FILETIME::default();
    let mut user = FILETIME::default();
    // SAFETY: the process handle is live for as long as `child` is, and the
    // four structures are valid for writing.
    let known = unsafe {
        GetProcessTimes(
            child.as_raw_handle().cast(),
            &raw mut created,
            &raw mut exited,
            &raw mut kernel,
            &raw mut user,
        )
    };
    // Both are counts of 100 nanoseconds.
    let ticks =
        |time: FILETIME| u64::from(time.dwHighDateTime) << 32 | u64::from(time.dwLowDateTime);
    (known != 0)
        .then(|| std::time::Duration::from_nanos((ticks(kernel) + ticks(user)).saturating_mul(100)))
}

#[cfg(windows)]
mod platform {
    use std::io;
    use std::process::Child;

    use windows_sys::Win32::Foundation::{
        CloseHandle, ERROR_INSUFFICIENT_BUFFER, ERROR_SUCCESS, HANDLE, INVALID_HANDLE_VALUE,
    };
    use windows_sys::Win32::NetworkManagement::IpHelper::{
        GetExtendedTcpTable, TCP_TABLE_OWNER_PID_ALL,
    };
    use windows_sys::Win32::System::Diagnostics::ToolHelp::{
        CreateToolhelp32Snapshot, TH32CS_SNAPTHREAD, THREADENTRY32, Thread32First, Thread32Next,
    };
    use windows_sys::Win32::System::Threading::{
        GetProcessIdOfThread, OpenThread, ResumeThread, SuspendThread,
        THREAD_QUERY_LIMITED_INFORMATION, THREAD_SUSPEND_RESUME,
    };

    /// How many times the threads are listed before giving up on a process
    /// that keeps showing threads that cannot be held. Two is the ordinary
    /// case: one to suspend them, one to see that there are no others.
    const PASSES: usize = 6;
    /// The address family of the engine's socket (`AF_INET`).
    const IPV4: u32 = 2;
    /// The states of a row in the TCP table that are not a connection:
    /// closed, listening, and two on the way out (`MIB_TCP_STATE_CLOSED`,
    /// `_LISTEN`, `_TIME_WAIT` and `_DELETE_TCB`).
    const NO_CONNECTION: [u32; 4] = [1, 2, 11, 12];
    /// How often, and how long apart, a thread that another program holds
    /// suspended is looked at again before the process is given up: a tenth
    /// of a second in all.
    const HELD_TRIES: usize = 20;
    const HELD_WAIT: std::time::Duration = std::time::Duration::from_millis(5);
    /// What `SuspendThread` and `ResumeThread` return when they fail.
    const FAILED: u32 = u32::MAX;

    /// Suspended threads: their identifiers and the handles they are held by.
    #[derive(Debug)]
    pub(super) struct Threads(Vec<(u32, HANDLE)>);

    // SAFETY: a thread handle may be used and closed from any thread.
    unsafe impl Send for Threads {}

    pub(super) fn suspend(child: &Child) -> io::Result<Threads> {
        let process = child.id();
        let mut held = Threads(Vec::new());
        for _ in 0..PASSES {
            let mut added = false;
            let mut missed = false;
            for thread in threads_of(process)? {
                if held.0.iter().any(|(id, _)| *id == thread) {
                    continue;
                }
                match suspend_thread(process, thread) {
                    Some(handle) => {
                        held.0.push((thread, handle));
                        added = true;
                    }
                    // Usually a thread that ended since the list was made;
                    // the next list tells.
                    None => missed = true,
                }
            }
            if !added && !missed {
                return if held.0.is_empty() {
                    Err(io::Error::other("the process has no threads"))
                } else {
                    Ok(held)
                };
            }
        }
        // Dropping `held` resumes what was suspended.
        Err(io::Error::other(
            "its threads could not all be suspended, so none was left suspended",
        ))
    }

    /// The identifiers of the threads of `process` at this moment.
    fn threads_of(process: u32) -> io::Result<Vec<u32>> {
        let mut threads = Vec::new();
        // SAFETY: the snapshot handle is checked, used only here and closed
        // before returning; `entry` is a plain structure whose size field is
        // set as the functions require, and they write nothing beyond it.
        unsafe {
            let snapshot = CreateToolhelp32Snapshot(TH32CS_SNAPTHREAD, 0);
            if snapshot == INVALID_HANDLE_VALUE {
                return Err(io::Error::last_os_error());
            }
            let mut entry = THREADENTRY32 {
                dwSize: u32::try_from(size_of::<THREADENTRY32>()).unwrap_or(0),
                ..Default::default()
            };
            let mut more = Thread32First(snapshot, &raw mut entry);
            while more != 0 {
                if entry.th32OwnerProcessID == process {
                    threads.push(entry.th32ThreadID);
                }
                more = Thread32Next(snapshot, &raw mut entry);
            }
            CloseHandle(snapshot);
        }
        Ok(threads)
    }

    /// Suspends one thread of `process` and returns the handle it is held by.
    /// `None` if the thread is gone or may not be touched.
    fn suspend_thread(process: u32, thread: u32) -> Option<HANDLE> {
        // SAFETY: the handle is checked before use and closed here unless it
        // is returned, in which case `Threads` closes it. The thread may have
        // ended and its identifier been given to a thread of another program
        // since the list was made, so nothing is suspended before Windows has
        // confirmed whose thread the handle is for.
        unsafe {
            let handle = OpenThread(
                THREAD_SUSPEND_RESUME | THREAD_QUERY_LIMITED_INFORMATION,
                0,
                thread,
            );
            if handle.is_null() {
                return None;
            }
            if GetProcessIdOfThread(handle) != process || SuspendThread(handle) == FAILED {
                CloseHandle(handle);
                return None;
            }
            Some(handle)
        }
    }

    impl Threads {
        /// Resumes and lets go of every thread. Safe to call again: there is
        /// then nothing left to resume.
        pub(super) fn resume(&mut self) -> io::Result<()> {
            let mut outcome = Ok(());
            for (_, handle) in self.0.drain(..) {
                // SAFETY: the handle came from `OpenThread` with the right to
                // suspend and resume, and is closed exactly once, here.
                unsafe {
                    match ResumeThread(handle) {
                        // The count before the call: one suspension, ours.
                        // None, if some other program resumed it already.
                        0 | 1 => {}
                        FAILED => outcome = Err(io::Error::last_os_error()),
                        _ if let_go(handle) => {}
                        _ => {
                            outcome =
                                Err(io::Error::other("a thread is held suspended by another"));
                        }
                    }
                    CloseHandle(handle);
                }
            }
            outcome
        }
    }

    /// Whether a thread that some other program has suspended as well runs
    /// again within a moment. Programs that look into others, such as virus
    /// scanners and profilers, stop a thread for an instant; that is no
    /// reason to give a process up.
    ///
    /// # Safety
    ///
    /// `handle` must be a live thread handle with the right to suspend and
    /// resume.
    unsafe fn let_go(handle: HANDLE) -> bool {
        for _ in 0..HELD_TRIES {
            std::thread::sleep(HELD_WAIT);
            // SAFETY: as the caller promises. Suspending is the only way to
            // read the count, and is undone at once, so the thread is left
            // as it was found.
            let held = unsafe {
                let before = SuspendThread(handle);
                if before != FAILED {
                    ResumeThread(handle);
                }
                before
            };
            if held == 0 {
                return true;
            }
        }
        false
    }

    impl Drop for Threads {
        fn drop(&mut self) {
            let _ = self.resume();
        }
    }

    pub(super) fn has_connection(process: u32) -> Option<bool> {
        /// The table is a count followed by rows of this many numbers:
        /// state, local address and port, remote address and port, owner.
        const ROW: usize = 6;
        const STATE: usize = 0;
        const OWNER: usize = 5;
        // A size that will do unless the PC has hundreds of connections;
        // Windows says how much it needs if it is more.
        let mut table = vec![0u32; 1 + ROW * 256];
        for _ in 0..4 {
            let mut bytes = u32::try_from(table.len() * size_of::<u32>()).ok()?;
            // SAFETY: `table` is writable for the `bytes` passed, and aligned
            // for the rows, which consist of 32-bit numbers only.
            let outcome = unsafe {
                GetExtendedTcpTable(
                    table.as_mut_ptr().cast(),
                    &raw mut bytes,
                    0,
                    IPV4,
                    TCP_TABLE_OWNER_PID_ALL,
                    0,
                )
            };
            match outcome {
                ERROR_SUCCESS => {
                    let rows = usize::try_from(*table.first()?).ok()?;
                    let open = table.get(1..)?.chunks_exact(ROW).take(rows).any(|row| {
                        row.get(OWNER) == Some(&process)
                            && row
                                .get(STATE)
                                .is_some_and(|state| !NO_CONNECTION.contains(state))
                    });
                    return Some(open);
                }
                ERROR_INSUFFICIENT_BUFFER => {
                    // Room to spare, for connections made in the meantime.
                    let words = usize::try_from(bytes).ok()? / size_of::<u32>();
                    table.resize(words + 1 + ROW * 64, 0);
                }
                _ => return None,
            }
        }
        None
    }
}

#[cfg(not(windows))]
mod platform {
    use std::io;
    use std::process::Child;

    #[derive(Debug)]
    pub(super) struct Threads;

    impl Threads {
        pub(super) fn resume(&mut self) -> io::Result<()> {
            Ok(())
        }
    }

    pub(super) fn suspend(_: &Child) -> io::Result<Threads> {
        Err(io::ErrorKind::Unsupported.into())
    }

    pub(super) fn has_connection(_: u32) -> Option<bool> {
        None
    }
}

#[cfg(all(test, windows))]
mod tests {
    use std::net::{Ipv4Addr, TcpListener, TcpStream};
    use std::process::{Command, Stdio};
    use std::thread;
    use std::time::{Duration, Instant};

    use super::*;
    use crate::job::KillOnDrop;
    use crate::process::hide_console;

    /// A harmless process that works without pause: the command interpreter
    /// in an endless empty loop. Ended when the test drops it.
    struct Busy(Child);

    impl Drop for Busy {
        fn drop(&mut self) {
            let _ = self.0.kill();
            let _ = self.0.wait();
        }
    }

    fn busy() -> io::Result<Busy> {
        let mut command = Command::new("cmd");
        command
            .args(["/d", "/c", "for /l %i in (0,0,1) do @rem"])
            .stdin(Stdio::null())
            .stdout(Stdio::null())
            .stderr(Stdio::null());
        hide_console(&mut command);
        let child = Busy(command.spawn()?);
        // Under way once it has used some processor time.
        let worked = |child: &Busy| cpu_time(&child.0).unwrap_or_default();
        let started = Instant::now();
        while worked(&child) < Duration::from_millis(20) {
            if started.elapsed() > Duration::from_secs(20) {
                return Err(io::Error::other("the stand-in process does no work"));
            }
            thread::sleep(Duration::from_millis(5));
        }
        Ok(child)
    }

    /// The processor time `child` uses in the next fifth of a second.
    fn work_of(child: &Child) -> Duration {
        let before = cpu_time(child).unwrap_or_default();
        thread::sleep(Duration::from_millis(200));
        cpu_time(child).unwrap_or_default().saturating_sub(before)
    }

    /// Whether `child` ends within a few seconds.
    fn ends(child: &mut Child) -> bool {
        let started = Instant::now();
        while started.elapsed() < Duration::from_secs(10) {
            if matches!(child.try_wait(), Ok(Some(_))) {
                return true;
            }
            thread::sleep(Duration::from_millis(10));
        }
        false
    }

    #[test]
    fn a_suspended_process_does_no_work_until_it_is_resumed() -> io::Result<()> {
        let mut child = busy()?;
        assert!(work_of(&child.0) > Duration::ZERO);

        let suspended = suspend(&child.0)?;
        // A thread stops at the next moment Windows can stop it, not before
        // `suspend` returns.
        thread::sleep(Duration::from_millis(50));
        assert_eq!(work_of(&child.0), Duration::ZERO);
        // Stopped is not ended.
        assert!(matches!(child.0.try_wait(), Ok(None)));

        suspended.resume()?;
        assert!(work_of(&child.0) > Duration::ZERO);
        Ok(())
    }

    #[test]
    fn a_process_runs_again_when_its_suspension_is_dropped() -> io::Result<()> {
        let child = busy()?;
        drop(suspend(&child.0)?);
        assert!(work_of(&child.0) > Duration::ZERO);
        Ok(())
    }

    #[test]
    fn suspending_twice_takes_resuming_twice() -> io::Result<()> {
        let child = busy()?;
        let first = suspend(&child.0)?;
        let second = suspend(&child.0)?;
        // The first to resume finds the threads still held by the second,
        // which is how an engine suspended by some other program is noticed.
        assert!(first.resume().is_err());
        thread::sleep(Duration::from_millis(50));
        assert_eq!(work_of(&child.0), Duration::ZERO);
        second.resume()?;
        assert!(work_of(&child.0) > Duration::ZERO);
        Ok(())
    }

    #[test]
    fn a_thread_another_holds_for_a_moment_is_waited_for() -> io::Result<()> {
        let child = busy()?;
        let ours = suspend(&child.0)?;
        let theirs = suspend(&child.0)?;
        let passing = thread::spawn(move || {
            thread::sleep(Duration::from_millis(10));
            drop(theirs);
        });
        ours.resume()?;
        assert!(work_of(&child.0) > Duration::ZERO);
        let _ = passing.join();
        Ok(())
    }

    #[test]
    fn a_connection_is_told_from_a_socket_that_listens() -> io::Result<()> {
        // The stand-in has no sockets at all. This process has, and other
        // tests in it connect too, so here only "yes" can be asserted.
        let child = busy()?;
        assert_eq!(has_connection(child.0.id()), Some(false));
        let listener = TcpListener::bind((Ipv4Addr::LOCALHOST, 0))?;
        let _made = TcpStream::connect(listener.local_addr()?)?;
        let _accepted = listener.accept()?;
        assert_eq!(has_connection(std::process::id()), Some(true));
        Ok(())
    }

    #[test]
    fn a_suspended_process_can_be_ended() -> io::Result<()> {
        let mut child = busy()?;
        let suspended = suspend(&child.0)?;
        child.0.kill()?;
        assert!(ends(&mut child.0));
        // Its threads are gone; there is nothing to resume and no harm in trying.
        drop(suspended);
        Ok(())
    }

    #[test]
    fn a_suspended_process_ends_with_its_job() -> io::Result<()> {
        let mut child = busy()?;
        let job = KillOnDrop::attach(&child.0);
        assert!(job.is_some());
        let _suspended = suspend(&child.0)?;
        // What happens when Vinput exits or is killed: its handle to the job
        // closes.
        drop(job);
        assert!(ends(&mut child.0));
        Ok(())
    }
}
