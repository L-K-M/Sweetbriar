//! Thread refusal, for tests of what code does when the OS won't start
//! a thread. Linux only: there the soft `RLIMIT_NPROC` limit of 0
//! refuses new threads to an unprivileged process while existing ones
//! keep running.

/// Run `f` while this process can't start threads, then restore the
/// limit. It applies to the whole process, so run it in a child process
/// of its own (no parallel tests), and check inside `f` that a spawn
/// really fails: a privileged process ignores the limit.
pub fn with_new_threads_refused(f: impl FnOnce()) {
    /// Puts the old limit back, even if `f` panics.
    struct Restore(libc::rlimit);

    impl Drop for Restore {
        fn drop(&mut self) {
            // SAFETY: `self.0` is a valid `rlimit` read by getrlimit.
            let restored = unsafe { libc::setrlimit(libc::RLIMIT_NPROC, &self.0) };
            assert_eq!(restored, 0, "cannot restore RLIMIT_NPROC");
        }
    }

    let mut old = libc::rlimit {
        rlim_cur: 0,
        rlim_max: 0,
    };
    // SAFETY: `old` is a valid, writable `rlimit`.
    assert_eq!(unsafe { libc::getrlimit(libc::RLIMIT_NPROC, &mut old) }, 0);
    let restore = Restore(old);
    let refused = libc::rlimit {
        rlim_cur: 0,
        rlim_max: old.rlim_max,
    };
    // SAFETY: `refused` is a valid `rlimit`; lowering the soft limit
    // needs no privilege.
    assert_eq!(unsafe { libc::setrlimit(libc::RLIMIT_NPROC, &refused) }, 0);
    f();
    drop(restore);
}
