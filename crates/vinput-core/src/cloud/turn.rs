//! Taking turns across processes.
//!
//! A refresh token is traded for the next one, and the old one then stops
//! working. Two Vinput processes that share the credential store, the
//! resident one and a `vinput transcribe` started beside it, must therefore
//! never trade the same token at once: the server would withdraw what it
//! gave the first, and whichever wrote last would leave a dead token behind.
//! So reading the token, trading it and keeping the next one happen inside a
//! [`Turn`], which only one process holds at a time, under a name Windows
//! knows them all by.
#![allow(unsafe_code)]

use std::time::Duration;

/// A turn at something only one Vinput process may do at a time. It ends
/// when this is dropped, on the thread that took it.
pub(super) struct Turn {
    #[cfg_attr(
        not(test),
        expect(dead_code, reason = "held for what dropping it does")
    )]
    held: platform::Held,
}

impl Turn {
    /// Waits for the turn called `name`, for `wait` at most. A process
    /// that cannot get it in that time goes ahead without: the other one is
    /// stuck, and waiting longer would only pass that on.
    pub(super) fn take(name: &str, wait: Duration) -> Self {
        Self {
            held: platform::Held::take(name, wait),
        }
    }

    /// Whether the turn was had, rather than given up waiting for.
    #[cfg(test)]
    pub(super) fn is_held(&self) -> bool {
        self.held.is_held()
    }
}

#[cfg(windows)]
mod platform {
    use std::ptr::null;
    use std::time::Duration;

    use windows_sys::Win32::Foundation::{CloseHandle, HANDLE, WAIT_ABANDONED, WAIT_OBJECT_0};
    use windows_sys::Win32::System::Threading::{CreateMutexW, ReleaseMutex, WaitForSingleObject};

    /// A named mutex of Windows, and whether this thread owns it.
    pub(super) struct Held {
        mutex: HANDLE,
        owned: bool,
    }

    impl Held {
        pub(super) fn take(name: &str, wait: Duration) -> Self {
            let name: Vec<u16> = name.encode_utf16().chain(std::iter::once(0)).collect();
            let wait = u32::try_from(wait.as_millis()).unwrap_or(u32::MAX - 1);
            // SAFETY: `name` is NUL-terminated and outlives the call. A null
            // handle, should Windows refuse, is never waited for or closed.
            let mutex = unsafe { CreateMutexW(null(), 0, name.as_ptr()) };
            let owned = !mutex.is_null() && {
                // SAFETY: `mutex` is a live handle of this process. A mutex
                // whose owner ended without releasing it is owned as well.
                let waited = unsafe { WaitForSingleObject(mutex, wait) };
                waited == WAIT_OBJECT_0 || waited == WAIT_ABANDONED
            };
            Self { mutex, owned }
        }

        #[cfg(test)]
        pub(super) fn is_held(&self) -> bool {
            self.owned
        }
    }

    impl Drop for Held {
        fn drop(&mut self) {
            if self.mutex.is_null() {
                return;
            }
            // SAFETY: the handle is ours and live; it is released only if
            // this thread owns it, and closed exactly once. The raw handle
            // makes this type stay on the thread that took the turn.
            unsafe {
                if self.owned {
                    ReleaseMutex(self.mutex);
                }
                CloseHandle(self.mutex);
            }
        }
    }
}

#[cfg(not(windows))]
mod platform {
    use std::time::Duration;

    /// Where Vinput does not run there is nobody to take turns with.
    pub(super) struct Held;

    impl Held {
        pub(super) fn take(_name: &str, _wait: Duration) -> Self {
            Self
        }

        #[cfg(test)]
        pub(super) fn is_held(&self) -> bool {
            true
        }
    }
}

#[cfg(all(test, windows))]
mod tests {
    use std::sync::Arc;
    use std::sync::atomic::{AtomicU32, Ordering};
    use std::time::Instant;

    use super::*;

    fn name(of: &str) -> String {
        format!("Local\\VinputTurnTest-{of}-{}", std::process::id())
    }

    #[test]
    fn only_one_holds_a_turn_at_a_time() {
        let name = name("one");
        let inside = Arc::new(AtomicU32::new(0));
        let most = Arc::new(AtomicU32::new(0));
        let threads: Vec<_> = (0..4)
            .map(|_| {
                let (name, inside, most) = (name.clone(), Arc::clone(&inside), Arc::clone(&most));
                std::thread::spawn(move || {
                    for _ in 0..20 {
                        let turn = Turn::take(&name, Duration::from_secs(10));
                        assert!(turn.is_held());
                        let now = inside.fetch_add(1, Ordering::SeqCst) + 1;
                        most.fetch_max(now, Ordering::SeqCst);
                        std::thread::yield_now();
                        inside.fetch_sub(1, Ordering::SeqCst);
                    }
                })
            })
            .collect();
        for thread in threads {
            assert!(thread.join().is_ok());
        }
        assert_eq!(most.load(Ordering::SeqCst), 1);
    }

    #[test]
    fn a_turn_that_never_comes_is_not_waited_for_forever() {
        let name = name("stuck");
        let held = Turn::take(&name, Duration::from_secs(1));
        assert!(held.is_held());
        let other = name.clone();
        let waited = std::thread::spawn(move || {
            let started = Instant::now();
            let turn = Turn::take(&other, Duration::from_millis(50));
            (turn.is_held(), started.elapsed())
        })
        .join();
        let Ok((had_it, took)) = waited else {
            unreachable!("the waiting thread ended badly");
        };
        assert!(!had_it);
        assert!(took < Duration::from_secs(5), "{took:?}");
        // Once it is given back, the next one has it at once.
        drop(held);
        let next = std::thread::spawn(move || Turn::take(&name, Duration::from_secs(1)).is_held());
        assert_eq!(next.join().ok(), Some(true));
    }
}
