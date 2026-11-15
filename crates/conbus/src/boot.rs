//! When this machine last booted, as the kernel says now.
//!
//! The daemon writes it on its start line, so a start can be told from a crash by
//! the log alone. The OS lists of past boots do not last: the macOS 27.2.1 update
//! on 2026-11-13 began `last reboot` and the power log afresh, and the canary then
//! counted the two boots before it as crashes.

use conbus_core::clock::Timestamp;

/// The boot this process runs in, or `None` where the kernel would not say.
pub fn booted() -> Option<Timestamp> {
    Timestamp::from_unix_seconds(boot_seconds()?)
}

#[cfg(target_os = "macos")]
fn boot_seconds() -> Option<i64> {
    let mut value = libc::timeval {
        tv_sec: 0,
        tv_usec: 0,
    };
    let mut size = std::mem::size_of::<libc::timeval>();
    // SAFETY: the name is a NUL-terminated literal, `value` is a live timeval and
    // `size` its size, which is what kern.boottime writes; no new value is set.
    let rc = unsafe {
        libc::sysctlbyname(
            c"kern.boottime".as_ptr(),
            (&raw mut value).cast(),
            &mut size,
            std::ptr::null_mut(),
            0,
        )
    };
    (rc == 0 && size == std::mem::size_of::<libc::timeval>() && value.tv_sec > 0)
        .then_some(value.tv_sec)
}

#[cfg(target_os = "linux")]
fn boot_seconds() -> Option<i64> {
    let stat = std::fs::read_to_string("/proc/stat").ok()?;
    stat.lines()
        .find_map(|line| line.strip_prefix("btime "))?
        .trim()
        .parse()
        .ok()
        .filter(|seconds| *seconds > 0)
}

#[cfg(not(any(target_os = "macos", target_os = "linux")))]
fn boot_seconds() -> Option<i64> {
    None
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_boot_is_in_the_past_and_after_this_code_was_written() {
        let booted = booted().expect("the kernel names its boot");
        let now = Timestamp::now();
        assert!(now.seconds_since(booted) >= 0, "{booted} is after {now}");
        let written = Timestamp::from_unix_seconds(1_793_000_000).expect("a time");
        assert!(
            booted.seconds_since(written) > 0,
            "{booted} is before 2026-10"
        );
    }
}
