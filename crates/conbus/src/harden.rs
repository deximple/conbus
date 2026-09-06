//! Keep derived state out of anything that outlives the process.
//!
//! The plan reversed an earlier file-backed mmap ring: a 0600 plaintext ring
//! file defeats both the envelope encryption and the capability token, because
//! any process with the same uid just reads it. Derived state therefore lives in
//! the daemon's own memory, and this module is what keeps that memory private.

/// What hardening actually achieved. Reported rather than assumed, because
/// `mlockall` needs privileges the daemon usually does not have — and on
/// Darwin it is not implemented at all (ENOSYS). Per-region `mlock(2)` is.
#[derive(Clone, PartialEq, Eq, Debug)]
pub struct Report {
    /// Core dumps disabled, so a crash cannot spill conversation state to disk.
    pub core_dumps_disabled: bool,
    /// Pages holding this process's private writable memory are locked into RAM.
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

/// Lock private writable pages again after the heap has grown (restore, a
/// long session). New anonymous pages from `HashMap` growth are otherwise
/// unlocked even when the first pass succeeded.
pub fn relock_derived() -> Report {
    harden()
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
        return (true, None);
    }
    let all_err = std::io::Error::last_os_error();
    match lock_private_writable() {
        Ok(n) if n > 0 => (true, None),
        Ok(_) => (
            false,
            Some(format!(
                "{all_err}; per-region mlock found no private writable pages"
            )),
        ),
        Err(why) => (false, Some(format!("{all_err}; {why}"))),
    }
}

/// Lock every private writable mapping we own. Darwin's `mlockall` is ENOSYS;
/// `mlock(2)` on a region is the documented substitute.
fn lock_private_writable() -> Result<usize, String> {
    #[cfg(target_os = "macos")]
    {
        darwin_lock_private_writable()
    }
    #[cfg(not(target_os = "macos"))]
    {
        Err("mlockall failed and this OS has no per-region fallback".into())
    }
}

#[cfg(target_os = "macos")]
fn darwin_lock_private_writable() -> Result<usize, String> {
    // Layout from <mach/vm_region.h> `vm_region_basic_info_64`.
    #[repr(C)]
    struct RegionInfo {
        protection: i32,
        max_protection: i32,
        inheritance: u32,
        shared: i32,
        reserved: i32,
        offset: u64,
        behavior: i32,
        user_wired_count: u16,
        _pad: u16,
    }

    const VM_REGION_BASIC_INFO_64: i32 = 9;
    const VM_PROT_WRITE: i32 = 0x02;
    // Skip giant mappings (shared caches, GPU); derived state is the heap.
    const MAX_LOCK: usize = 64 * 1024 * 1024;

    unsafe extern "C" {
        fn mach_vm_region(
            target_task: libc::mach_port_t,
            address: *mut u64,
            size: *mut u64,
            flavor: i32,
            info: *mut i32,
            info_count: *mut u32,
            object_name: *mut libc::mach_port_t,
        ) -> i32;
    }

    #[allow(deprecated)]
    let task = unsafe { libc::mach_task_self() };
    let mut address: u64 = 0;
    let mut locked = 0usize;
    let mut last_err = None;

    for _ in 0..16_384 {
        let mut size: u64 = 0;
        let mut info = RegionInfo {
            protection: 0,
            max_protection: 0,
            inheritance: 0,
            shared: 0,
            reserved: 0,
            offset: 0,
            behavior: 0,
            user_wired_count: 0,
            _pad: 0,
        };
        let mut count = (std::mem::size_of::<RegionInfo>() / std::mem::size_of::<i32>()) as u32;
        let mut object: libc::mach_port_t = 0;
        // SAFETY: pointers are to stack values that outlive the call; flavor
        // and count match `vm_region_basic_info_64`.
        let kr = unsafe {
            mach_vm_region(
                task,
                &mut address,
                &mut size,
                VM_REGION_BASIC_INFO_64,
                (&raw mut info).cast::<i32>(),
                &mut count,
                &mut object,
            )
        };
        if kr != 0 {
            break;
        }
        if size == 0 {
            break;
        }
        let writable = info.protection & VM_PROT_WRITE != 0;
        let private = info.shared == 0;
        let modest = (size as usize) <= MAX_LOCK;
        if writable && private && modest {
            // SAFETY: `address`/`size` name a mapping this process owns.
            let ok = unsafe { libc::mlock(address as *const libc::c_void, size as usize) } == 0;
            if ok {
                locked = locked.saturating_add(1);
            } else {
                last_err = Some(std::io::Error::last_os_error().to_string());
            }
        }
        let next = address.saturating_add(size);
        if next <= address {
            break;
        }
        address = next;
    }

    if locked == 0 {
        return Err(last_err.unwrap_or_else(|| "no region accepted mlock".into()));
    }
    Ok(locked)
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

    #[cfg(target_os = "macos")]
    #[test]
    fn darwin_locks_private_pages_when_mlockall_is_enosys() {
        let report = harden();
        assert!(
            report.memory_locked,
            "Darwin per-region mlock must succeed here: {report:?}"
        );
    }
}
