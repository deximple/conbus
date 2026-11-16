//! Conversation identity.

use std::fmt;
use std::str::FromStr;
use std::sync::{LazyLock, Mutex};

/// The conversation identity the bus owns. Vendor session IDs are aliases of it.
///
/// Rendered as a canonical UUID because Claude Code takes it verbatim as
/// `--session-id`, which requires a session UUID.
#[derive(Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub struct BusId(u128);

/// Length of the canonical hyphenated UUID text.
const CANONICAL_LEN: usize = 36;

impl BusId {
    /// Mint a fresh conversation identity.
    pub fn mint() -> Self {
        static CONTEXT: LazyLock<Mutex<uuid::ContextV7>> = LazyLock::new(|| Mutex::new(context()));
        BusId(uuid::Uuid::new_v7(uuid::Timestamp::now(&*CONTEXT)).as_u128())
    }
}

/// The context every id is minted from.
///
/// To the millisecond, the rest of a version 7 id is random, and ids minted by
/// two processes in the same millisecond sort either way. Each hook is a
/// process, and the spool drains in id order. The fraction of the millisecond
/// goes in too (RFC 9562 section 6.2, method 3), to a quarter of a microsecond.
/// It takes uuid 1.29.1: before 1.29 the version field overwrote the fraction's
/// top four bits, and 1.29.1 stopped the fraction overwriting the counter.
fn context() -> uuid::ContextV7 {
    uuid::ContextV7::new().with_additional_precision()
}

impl fmt::Display for BusId {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        fmt::Display::fmt(&uuid::Uuid::from_u128(self.0).hyphenated(), f)
    }
}

impl fmt::Debug for BusId {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        fmt::Display::fmt(self, f)
    }
}

/// A string that is not a canonical UUID.
#[derive(Debug, PartialEq, Eq)]
pub struct ParseBusIdError;

impl FromStr for BusId {
    type Err = ParseBusIdError;

    fn from_str(s: &str) -> Result<Self, Self::Err> {
        // Length-gate first: it is what rejects the simple, urn, and braced
        // spellings that `Uuid::try_parse` would otherwise accept.
        if s.len() != CANONICAL_LEN {
            return Err(ParseBusIdError);
        }
        uuid::Uuid::try_parse(s)
            .map(|u| BusId(u.as_u128()))
            .map_err(|_| ParseBusIdError)
    }
}

impl serde::Serialize for BusId {
    fn serialize<S: serde::Serializer>(&self, s: S) -> Result<S::Ok, S::Error> {
        s.collect_str(self)
    }
}

impl<'de> serde::Deserialize<'de> for BusId {
    fn deserialize<D: serde::Deserializer<'de>>(d: D) -> Result<Self, D::Error> {
        let text = <std::borrow::Cow<'de, str> as serde::Deserialize>::deserialize(d)?;
        text.parse().map_err(|_| {
            serde::de::Error::invalid_value(
                serde::de::Unexpected::Str(&text),
                &"a canonical hyphenated UUID",
            )
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn ids_minted_in_different_milliseconds_sort_in_mint_order() {
        // A regression guard, not a new behaviour: v7's leading timestamp is
        // why `bus_id` clusters by time in the log. Swapping to v4 breaks it.
        let first = BusId::mint();
        std::thread::sleep(std::time::Duration::from_millis(2));
        let second = BusId::mint();

        assert!(first < second, "{first} should sort before {second}");
    }

    #[test]
    fn every_mint_is_distinct() {
        let ids: std::collections::HashSet<BusId> = (0..1_000).map(|_| BusId::mint()).collect();

        assert_eq!(ids.len(), 1_000);
    }

    #[test]
    fn serialises_as_a_bare_uuid_string_so_the_log_stays_greppable() {
        let id = BusId::mint();

        let json = serde_json::to_string(&id).expect("serialise");

        assert_eq!(json, format!("\"{id}\""));
        assert_eq!(
            serde_json::from_str::<BusId>(&json).expect("deserialise"),
            id
        );
    }

    #[test]
    fn ids_from_two_processes_within_a_millisecond_sort_in_the_order_minted() {
        // Every hook is a process of its own with a context of its own, seeded
        // at random, and the spool drains in id order. Two hooks half a
        // millisecond apart landed in either order before the fraction of the
        // millisecond went into the id: rc.6's release run caught it.
        const SECONDS: u64 = 1_763_200_000;
        for _ in 0..1000 {
            let earlier =
                uuid::Uuid::new_v7(uuid::Timestamp::from_unix(context(), SECONDS, 100_000));
            let later = uuid::Uuid::new_v7(uuid::Timestamp::from_unix(context(), SECONDS, 600_000));
            assert!(earlier < later, "{earlier} sorts after {later}");
        }
    }

    #[test]
    fn parse_rejects_uuid_spellings_claude_code_would_not_accept() {
        // `--session-id` wants the canonical hyphenated form. Accepting the
        // simple or urn spellings here would let a malformed id travel all the
        // way to the vendor CLI before failing.
        let canonical = BusId::mint().to_string();
        let simple = canonical.replace('-', "");

        assert_eq!(simple.parse::<BusId>(), Err(ParseBusIdError));
        assert_eq!(
            format!("urn:uuid:{canonical}").parse::<BusId>(),
            Err(ParseBusIdError)
        );
        assert_eq!(
            format!("{{{canonical}}}").parse::<BusId>(),
            Err(ParseBusIdError)
        );
        assert_eq!("".parse::<BusId>(), Err(ParseBusIdError));
        assert_eq!("not-a-uuid".parse::<BusId>(), Err(ParseBusIdError));
    }

    #[test]
    fn minted_id_round_trips_through_its_canonical_uuid_text() {
        let id = BusId::mint();

        let text = id.to_string();
        let parsed: BusId = text.parse().expect("canonical text must parse back");

        assert_eq!(parsed, id);
    }
}
