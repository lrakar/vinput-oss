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
