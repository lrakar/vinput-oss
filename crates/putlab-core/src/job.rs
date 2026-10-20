//! Ties a helper process to the life of Vinput.
//!
//! A helper placed in a job object marked "kill on close" is ended by Windows
//! the moment the last handle to the job goes away, which happens when the
//! guard is dropped and also when Vinput itself exits or is killed. A helper
//! can therefore never be left running on its own.
#![allow(unsafe_code)]

use std::os::windows::io::AsRawHandle;
use std::process::Child;
use std::ptr::null;

use windows_sys::Win32::Foundation::{CloseHandle, HANDLE};
use windows_sys::Win32::System::JobObjects::{
    AssignProcessToJobObject, CreateJobObjectW, JOB_OBJECT_LIMIT_KILL_ON_JOB_CLOSE,
    JOBOBJECT_EXTENDED_LIMIT_INFORMATION, JobObjectExtendedLimitInformation,
    SetInformationJobObject,
};
use windows_sys::Win32::System::ProcessStatus::{K32GetProcessMemoryInfo, PROCESS_MEMORY_COUNTERS};
use windows_sys::Win32::System::Threading::{
    INFINITE, OpenProcess, PROCESS_SYNCHRONIZE, WaitForSingleObject,
};

/// Ends the process it was attached to when dropped.
#[derive(Debug)]
pub struct KillOnDrop(HANDLE);

// SAFETY: a job handle may be used and closed from any thread.
unsafe impl Send for KillOnDrop {}

impl KillOnDrop {
    /// Puts `child` in a new job. `None` if Windows refuses, in which case the
    /// caller still ends the process itself in the ordinary way.
    #[must_use]
    pub fn attach(child: &Child) -> Option<Self> {
        let mut limits = JOBOBJECT_EXTENDED_LIMIT_INFORMATION::default();
        limits.BasicLimitInformation.LimitFlags = JOB_OBJECT_LIMIT_KILL_ON_JOB_CLOSE;
        // SAFETY: the job is created unnamed with default security; `limits` is
        // a fully initialized structure of the size passed; the child handle is
        // live for as long as `child` is. The job handle is closed on drop, or
        // here if the job could not be set up.
        unsafe {
            let job = CreateJobObjectW(null(), null());
            if job.is_null() {
                return None;
            }
            let configured = SetInformationJobObject(
                job,
                JobObjectExtendedLimitInformation,
                (&raw const limits).cast(),
                u32::try_from(size_of::<JOBOBJECT_EXTENDED_LIMIT_INFORMATION>()).unwrap_or(0),
            );
            if configured == 0 || AssignProcessToJobObject(job, child.as_raw_handle().cast()) == 0 {
                CloseHandle(job);
                return None;
            }
            Some(Self(job))
        }
    }
}

impl Drop for KillOnDrop {
    fn drop(&mut self) {
        // SAFETY: the handle came from `CreateJobObjectW` and is closed once.
        unsafe { CloseHandle(self.0) };
    }
}

/// How many bytes of memory `child` holds right now; `None` if Windows does
/// not say.
#[must_use]
pub fn memory_of(child: &Child) -> Option<u64> {
    let mut counters = PROCESS_MEMORY_COUNTERS::default();
    let size = u32::try_from(size_of::<PROCESS_MEMORY_COUNTERS>()).unwrap_or(0);
    // SAFETY: the structure is of the size passed and the child handle is
    // live for as long as `child` is.
    let told =
        unsafe { K32GetProcessMemoryInfo(child.as_raw_handle().cast(), &raw mut counters, size) };
    (told != 0).then_some(counters.WorkingSetSize as u64)
}

/// The end of a helper process, which a thread can wait for without
/// holding the helper itself and without looking again and again.
#[derive(Debug)]
pub struct Exit(HANDLE);

// SAFETY: a process handle may be waited on and closed from any thread.
unsafe impl Send for Exit {}
// SAFETY: waiting on a handle changes nothing about it.
unsafe impl Sync for Exit {}

impl Exit {
    /// The end of `child`. `None` if Windows gives no handle to wait on.
    #[must_use]
    pub fn of(child: &Child) -> Option<Self> {
        // SAFETY: plain value arguments. The number names `child` for as
        // long as `child` is not waited for, which the caller has not done.
        let handle = unsafe { OpenProcess(PROCESS_SYNCHRONIZE, 0, child.id()) };
        (!handle.is_null()).then_some(Self(handle))
    }

    /// Returns once the process has ended, however it ended.
    pub fn wait(&self) {
        // SAFETY: the handle is open until this is dropped.
        unsafe { WaitForSingleObject(self.0, INFINITE) };
    }
}

impl Drop for Exit {
    fn drop(&mut self) {
        // SAFETY: the handle came from `OpenProcess` and is closed once.
        unsafe { CloseHandle(self.0) };
    }
}
