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
#[derive(Clone, Copy, PartialEq, Eq, PartialOrd, Ord)]
pub struct Timestamp(OffsetDateTime);

impl Timestamp {
    /// The current instant.
    pub fn now() -> Self {
        Timestamp(OffsetDateTime::now_utc())
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

#[cfg(test)]
mod tests {
    use super::*;

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
