//! Compile-level proofs that the risk gate has no bypass.
//!
//! D2.2 asks for a test proving no bypass path compiles. What is provable, and
//! what these cases establish:
//!
//! - The engine **owns** its venue and lends it out only immutably, so no caller
//!   can send an order or a cancel through it.
//! - The gate, the venue, and the order store are private fields.
//! - The order store is readable but not writable from outside, so an order the
//!   gate never approved cannot be pushed into the session's books.
//!
//! Together with the strategy cases in the `strategy` crate — which show a
//! strategy is handed nothing to reach with — that closes the paths from inside
//! a running session.
//!
//! # What this does not prove
//!
//! `oms::Order::new` and `VenueAdapter::submit` are public, so a caller who
//! constructs **its own** venue can of course send to it. That is not a bypass
//! of a session's gate; it is a different program. Making an `Order`
//! unforgeable without gate approval would need a token type that only `risk`
//! can mint, and a token that crosses crate boundaries unforgeably is a design
//! change worth its own discussion (#1).

#[test]
fn no_path_reaches_a_session_venue_around_the_gate() {
    trybuild::TestCases::new().compile_fail("tests/compile-fail/*.rs");
}
