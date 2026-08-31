//! The context a continuing session is handed.
//!
//! Built from the log and nothing else, so a pack can only ever say what was
//! actually recorded — which at the default retention level is the
//! conversation and the shape of its tool work, never the tool bodies.

/// How many UTF-16 code units a string is.
///
/// The unit is not a detail. Claude Code's 10,000-character cap is a JavaScript
/// `String.length` comparison, and `String.length` counts UTF-16 code units.
/// Both plausible substitutes are wrong, in opposite directions:
///
/// - Bytes over-count. `INLINE_CAP_BYTES` in `retention.rs` is a byte cap and
///   its idiom is right there to copy, but a Korean or Japanese pack measured in
///   bytes reads three times its real size, so a pack that would have fitted is
///   refused.
/// - Scalars under-count. Anything outside the basic plane — an emoji, a
///   mathematical letter — is one `char` and two UTF-16 units, so a pack
///   measured in `chars()` can pass a local check and still cross the vendor's
///   cap, which is the silent degradation this budget exists to make
///   unreachable.
pub fn units(text: &str) -> usize {
    text.encode_utf16().count()
}

/// The vendor's hard limit on hook output, in UTF-16 code units.
///
/// Past this the text does not fail, which would be survivable — it is written
/// to a file and the model is handed a path and a short preview instead. The
/// headers of a pack land in that preview, so the failure reads as success.
pub const VENDOR_CAP_UNITS: usize = 10_000;

/// What a pack builder is allowed to emit.
///
/// Far below the cap on purpose. The point is not to test the degradation path
/// but to make it unreachable, so that adding a section later cannot quietly
/// tip a pack over a line nobody is watching.
pub const PACK_BUDGET_UNITS: usize = 4_000;

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_budget_counts_the_way_the_vendor_counts() {
        // One test, two mistakes. Korean is one UTF-16 unit and three bytes, so
        // a byte count fails it. The astral character is two UTF-16 units and
        // one `char`, so a scalar count fails it too. Neither substitute can
        // pass this.
        assert_eq!(units("한"), 1, "bytes would say 3");
        assert_eq!("한".len(), 3);

        assert_eq!(units("\u{1D11E}"), 2, "chars() would say 1");
        assert_eq!("\u{1D11E}".chars().count(), 1);

        assert_eq!(units("ok"), 2);
        assert_eq!(units(""), 0);
    }

    #[test]
    fn the_pack_budget_leaves_the_degradation_path_out_of_reach() {
        // A budget close to the cap makes the silent failure one added section
        // away.
        const { assert!(PACK_BUDGET_UNITS * 2 < VENDOR_CAP_UNITS) };
    }
}
