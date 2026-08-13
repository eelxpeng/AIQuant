//! TEST-022 — the value types keep the widths ADR #4's cache argument assumes.
//! TEST-023 — `types` depends on no other workspace crate.
//!
//! Proves S6 / INV-011 / INV-012.

use std::mem::size_of;

use types::{ExchangeSpan, ExchangeTime, Notional, Px, Qty, ReceiveTime};

/// Read at compile time, so this test performs no I/O and does not depend on
/// the working directory.
const MANIFEST: &str = include_str!("../Cargo.toml");

// --- TEST-022 -------------------------------------------------------------

/// ADR #4 rejected "`i128` for everything" because a top-of-book record with
/// two prices and two sizes would go from 32 bytes to 64, halving L1 density.
/// That argument is only valid while these hold.
#[test]
fn value_type_sizes_are_pinned() {
    assert_eq!(size_of::<Px>(), 8);
    assert_eq!(size_of::<Qty>(), 8);
    assert_eq!(size_of::<Notional>(), 16);

    // A phantom clock kind is zero-sized: typing the clocks costs no memory.
    assert_eq!(size_of::<ExchangeTime>(), 8);
    assert_eq!(size_of::<ReceiveTime>(), 8);
    assert_eq!(size_of::<ExchangeSpan>(), 16);

    // The four-price/four-size top-of-book record ADR #4 costs out.
    assert_eq!(size_of::<[Px; 2]>() + size_of::<[Qty; 2]>(), 32);
}

// --- TEST-023 -------------------------------------------------------------

/// Asserting *zero* normal dependencies is stronger than "no workspace crate",
/// and it is the truth for this crate — `proptest` and `trybuild` are
/// dev-dependencies and do not appear in this table.
///
/// Known gap (research.md R-008): a text assertion cannot see a dependency
/// introduced through a workspace-inherited table. Closing that needs a
/// `cargo tree` / `cargo deny` check in CI, which is D0.4 (→ #1). Do not
/// over-trust this test.
#[test]
fn types_has_no_workspace_dependency() {
    let mut in_dependencies = false;

    for line in MANIFEST.lines() {
        let trimmed = line.trim();

        if trimmed.starts_with('[') {
            in_dependencies = trimmed == "[dependencies]";
            continue;
        }

        if in_dependencies && !trimmed.is_empty() && !trimmed.starts_with('#') {
            panic!("`types` must be the bottom of the dependency order, found: {trimmed}");
        }
    }

    assert!(
        MANIFEST.contains("[dependencies]"),
        "the manifest must keep an explicit empty [dependencies] table, \
         so that adding one is a visible diff"
    );
}
