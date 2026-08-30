//! Conversation identity.

use std::fmt;
use std::str::FromStr;

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
        BusId(uuid::Uuid::now_v7().as_u128())
    }
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
