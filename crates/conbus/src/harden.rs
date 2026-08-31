//! Keep derived state out of anything that outlives the process.
//!
//! The plan reversed an earlier file-backed mmap ring: a 0600 plaintext ring
//! file defeats both the envelope encryption and the capability token, because
//! any process with the same uid just reads it. Derived state therefore lives in
//! the daemon's own memory, and this module is what keeps that memory private.

/// What hardening actually achieved. Reported rather than assumed, because
/// `mlockall` needs privileges the daemon usually does not have.
#[derive(Clone, PartialEq, Eq, Debug)]
pub struct Report {
    /// Core dumps disabled, so a crash cannot spill conversation state to disk.
    pub core_dumps_disabled: bool,
    /// Pages locked into RAM, so state cannot reach swap.
    pub memory_locked: bool,
    /// Why locking failed, when it did.
    pub memory_lock_error: Option<String>,
}

/// Harden the current process. Never fails: a daemon that will not start
/// because it could not lock memory is worse than one that says so.
pub fn harden() -> Report {
    let (memory_locked, memory_lock_error) = lock_memory();
    Report {
        core_dumps_disabled: disable_core_dumps(),
        memory_locked,
        memory_lock_error,
    }
}

/// Drop the core dump limit to zero, hard limit included, so it cannot be
/// raised again by anything that gets into this process later.
fn disable_core_dumps() -> bool {
    let limit = libc::rlimit {
        rlim_cur: 0,
        rlim_max: 0,
    };
    // SAFETY: `limit` is a fully initialised `rlimit` and the resource is valid.
    unsafe { libc::setrlimit(libc::RLIMIT_CORE, &limit) == 0 }
}

fn lock_memory() -> (bool, Option<String>) {
    // SAFETY: no arguments to get wrong; the call reports failure in errno.
    let locked = unsafe { libc::mlockall(libc::MCL_CURRENT | libc::MCL_FUTURE) } == 0;
    if locked {
        (true, None)
    } else {
        (false, Some(std::io::Error::last_os_error().to_string()))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn hardening_disables_core_dumps() {
        let report = harden();

        assert!(
            report.core_dumps_disabled,
            "core dumps must be off: {report:?}"
        );

        let mut limit = unsafe { std::mem::zeroed::<libc::rlimit>() };
        let read = unsafe { libc::getrlimit(libc::RLIMIT_CORE, &mut limit) };
        assert_eq!(read, 0, "getrlimit failed");
        assert_eq!(limit.rlim_cur, 0, "the kernel still allows a core dump");
    }

    #[test]
    fn a_failure_to_lock_memory_is_reported_rather_than_hidden() {
        let report = harden();

        assert_eq!(
            report.memory_locked,
            report.memory_lock_error.is_none(),
            "either memory is locked or there is a reason why not: {report:?}"
        );
    }
}
