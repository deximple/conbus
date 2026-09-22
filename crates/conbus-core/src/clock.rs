//! Record timestamps.

use std::fmt;
use std::str::FromStr;

use time::format_description::BorrowedFormatItem;
use time::macros::format_description;
use time::{OffsetDateTime, PrimitiveDateTime};

/// Fixed-width RFC 3339 in UTC: `2026-08-30T07:41:02.481Z`.
///
/// Fixed width is the point. `time`'s own RFC 3339 emits a variable number of
/// subsecond digits, which would break the lexicographic ordering that makes a
/// `grep`-ed segment readable in append order.
const FORMAT: &[BorrowedFormatItem<'_>] =
    format_description!("[year]-[month]-[day]T[hour]:[minute]:[second].[subsecond digits:3]Z");

/// A UTC instant, rendered fixed-width so that log lines sort lexicographically
/// in the same order they were appended.
///
/// Fixed width is only half of it: the timestamp is also declared before `seq`
/// on the record, because `seq` is variable-width and a text sort would
/// otherwise put 10 before 2. Both halves are needed for the claim to hold, and
/// it holds up to a wall clock that steps backwards.
#[derive(Clone, Copy, PartialEq, Eq, PartialOrd, Ord)]
pub struct Timestamp(OffsetDateTime);

impl Timestamp {
    /// The current instant, truncated to the precision it is written at.
    ///
    /// Truncating here rather than at serialisation keeps one invariant that
    /// would otherwise be quietly false: a timestamp compares the same before
    /// and after a round trip through the log. Without it an in-memory value
    /// carries nanoseconds the format drops, so a record read back from disk
    /// can sort before a value taken earlier in the same millisecond.
    pub fn now() -> Self {
        let now = OffsetDateTime::now_utc();
        let millis = now.millisecond();
        Timestamp(
            now.replace_nanosecond(u32::from(millis) * 1_000_000)
                .unwrap_or(now),
        )
    }

    /// A timestamp from unix seconds, which is how Codex's thread APIs
    /// report time (as a number on a turn, as a string on a thread).
    pub fn from_unix_seconds(seconds: i64) -> Option<Self> {
        OffsetDateTime::from_unix_timestamp(seconds)
            .ok()
            .map(Timestamp)
    }

    /// Whole seconds from `earlier` to this instant. Negative when this one
    /// came first, which is a real case rather than a bug: a backfilled record
    /// carries the time it happened, so a conversation recovered today can be
    /// months older than the session asking about it.
    pub fn seconds_since(&self, earlier: Timestamp) -> i64 {
        (self.0 - earlier.0).whole_seconds()
    }
}

impl fmt::Display for Timestamp {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        // The format is total over every `OffsetDateTime`, so the only way this
        // can fail is a formatter write error.
        let text = self.0.format(FORMAT).map_err(|_| fmt::Error)?;
        f.write_str(&text)
    }
}

impl fmt::Debug for Timestamp {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        fmt::Display::fmt(self, f)
    }
}

/// A string that is not a Conbus timestamp.
#[derive(Debug, PartialEq, Eq)]
pub struct ParseTimestampError;

impl FromStr for Timestamp {
    type Err = ParseTimestampError;

    fn from_str(s: &str) -> Result<Self, Self::Err> {
        // The trailing `Z` is a literal in `FORMAT`, not an offset component, so
        // the parse yields a primitive datetime that we then pin to UTC.
        let parsed = PrimitiveDateTime::parse(s, FORMAT).map_err(|_| ParseTimestampError)?;
        Ok(Timestamp(parsed.assume_utc()))
    }
}

impl serde::Serialize for Timestamp {
    fn serialize<S: serde::Serializer>(&self, s: S) -> Result<S::Ok, S::Error> {
        s.collect_str(self)
    }
}

impl<'de> serde::Deserialize<'de> for Timestamp {
    fn deserialize<D: serde::Deserializer<'de>>(d: D) -> Result<Self, D::Error> {
        let text = <std::borrow::Cow<'de, str> as serde::Deserialize>::deserialize(d)?;
        text.parse().map_err(|_| {
            serde::de::Error::invalid_value(
                serde::de::Unexpected::Str(&text),
                &"a fixed-width UTC timestamp such as 2026-08-30T07:41:02.481Z",
            )
        })
    }
}

/// How long ago, the way a person says it: "42s", "7m", "3h", "14d".
///
/// Whole units, rounded down, because every caller is answering "is this
/// current?" — and "0s" for anything in the future, which a clock that stepped
/// backwards can produce.
pub fn ago(seconds: i64) -> String {
    let seconds = seconds.max(0);
    match seconds {
        0..60 => format!("{seconds}s"),
        60..3_600 => format!("{}m", seconds / 60),
        3_600..86_400 => format!("{}h", seconds / 3_600),
        _ => format!("{}d", seconds / 86_400),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn ages_read_the_way_a_person_says_them() {
        assert_eq!(ago(42), "42s");
        assert_eq!(ago(7 * 60), "7m");
        assert_eq!(ago(3 * 3_600 + 5), "3h");
        assert_eq!(ago(14 * 86_400), "14d");
        assert_eq!(ago(-3), "0s");
    }

    #[test]
    fn a_timestamp_compares_the_same_before_and_after_the_log() {
        // The format writes milliseconds. If `now()` kept nanoseconds, a value
        // read back would sort before one taken earlier in the same
        // millisecond, and every comparison across the log boundary would be
        // subtly wrong.
        let taken = Timestamp::now();

        let round_tripped: Timestamp = taken.to_string().parse().expect("parse");

        assert_eq!(round_tripped, taken);
    }

    #[test]
    fn serialises_as_a_bare_string() {
        let ts: Timestamp = "2026-08-30T07:41:02.481Z".parse().expect("parse");

        let json = serde_json::to_string(&ts).expect("serialise");

        assert_eq!(json, "\"2026-08-30T07:41:02.481Z\"");
        assert_eq!(
            serde_json::from_str::<Timestamp>(&json).expect("deserialise"),
            ts
        );
    }

    #[test]
    fn rejects_text_that_is_not_a_conbus_timestamp() {
        assert_eq!("".parse::<Timestamp>(), Err(ParseTimestampError));
        assert_eq!("2026-08-30".parse::<Timestamp>(), Err(ParseTimestampError));
        // `time`'s own RFC 3339 spelling: variable subseconds, offset instead of Z.
        assert_eq!(
            "2026-08-30T07:41:02.481234+00:00".parse::<Timestamp>(),
            Err(ParseTimestampError)
        );
    }

    #[test]
    fn text_order_matches_chronological_order() {
        // The guard behind the fixed-width format: sorting segment lines as text
        // must give append order.
        let earlier: Timestamp = "2026-08-30T07:41:02.481Z".parse().expect("parse");
        let later: Timestamp = "2026-08-30T07:41:02.482Z".parse().expect("parse");
        let much_later: Timestamp = "2026-09-01T00:00:00.000Z".parse().expect("parse");

        assert!(earlier < later && later < much_later);
        assert!(earlier.to_string() < later.to_string());
        assert!(later.to_string() < much_later.to_string());
    }

    #[test]
    fn renders_fixed_width_utc_so_log_lines_sort_lexicographically() {
        let text = Timestamp::now().to_string();

        assert_eq!(text.len(), 24, "{text} is not fixed width");
        assert!(text.ends_with('Z'), "{text} is not UTC");
        assert_eq!(&text[10..11], "T");
        assert_eq!(&text[19..20], ".");
    }
}
