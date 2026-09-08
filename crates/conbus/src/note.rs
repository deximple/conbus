//! One stamped line of the daemon's log.
//!
//! launchd points both of the daemon's streams at `~/.conbus/daemon.log` and
//! appends across every restart. On 2026-09-08 that file held 691 lines and
//! not one time on any of them, so the five minutes when hooks stopped being
//! answered could not be placed against the drop log, the corpus, or a
//! restart — the outage was legible everywhere except in the daemon's own
//! account of itself.
//!
//! The stamp is the same fixed-width UTC the records carry, so the two sort
//! together and a reader can grep one against the other.

use std::fmt::Arguments;
use std::io::Write;

use conbus_core::clock::Timestamp;

/// The text of one line, apart from when it is written.
///
/// Split out so the format can be tested without a clock: a test that needs
/// `Timestamp::now()` to assert on its own output can only check a shape.
pub fn line(at: Timestamp, message: Arguments<'_>) -> String {
    format!("{at} {message}\n")
}

/// Write one line to stderr, stamped, in a single call.
///
/// One `write_all` rather than `eprintln!`'s several, because two daemons —
/// the live one and a `conbus doctor` probe — can hold the same appended file
/// open, and a line assembled in pieces is a line that can be cut in half by
/// somebody else's.
pub fn say(message: Arguments<'_>) {
    let text = line(Timestamp::now(), message);
    let _ = std::io::stderr().write_all(text.as_bytes());
}

/// Everything the daemon says goes through here.
///
/// Not `eprintln!`. `nothing_in_the_daemon_speaks_around_the_stamped_channel`
/// fails the build if a bare one comes back.
macro_rules! note {
    ($($arg:tt)*) => {
        crate::note::say(format_args!("conbus daemon: {}", format_args!($($arg)*)))
    };
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_line_carries_the_time_it_happened() {
        let at: Timestamp = "2026-09-08T10:21:09.481Z".parse().expect("ts");

        let text = line(
            at,
            format_args!("conbus daemon: connection failed: {}", "boom"),
        );

        assert_eq!(
            text,
            "2026-09-08T10:21:09.481Z conbus daemon: connection failed: boom\n"
        );
    }

    #[test]
    fn the_stamp_is_fixed_width_so_daemon_log_sorts_the_way_the_record_log_does() {
        let early: Timestamp = "2026-09-08T10:21:09.481Z".parse().expect("ts");
        let late: Timestamp = "2026-09-08T10:26:28.002Z".parse().expect("ts");

        let first = line(early, format_args!("first"));
        let second = line(late, format_args!("second"));

        assert_eq!(first.find(' '), Some(24), "{first:?}");
        assert_eq!(second.find(' '), Some(24), "{second:?}");
        assert!(first < second, "a text sort must give the order of events");
        assert_eq!(
            first[..24].parse::<Timestamp>().expect("parses back"),
            early
        );
    }
}
