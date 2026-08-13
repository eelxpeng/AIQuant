//! Compile-level proofs for the strategy seam.
//!
//! D3.1 asks that the strategy trait expose no clock, socket, or filesystem,
//! *enforced by a compile-level test*. D3.4 asks that a strategy cannot observe
//! data timestamped after the event it is processing. Both are properties of
//! what the interface hands over, so both are provable by showing that the
//! reaching code does not build.
//!
//! Every case carries a legal use of the same type beside the illegal one. A
//! case that failed because a name was misspelled would prove nothing, and the
//! checked-in `.stderr` files are what make the difference visible: each must
//! fail with the intended error, not with "no method named ...".
//!
//! # What this cannot prove
//!
//! Rust cannot stop a strategy implementation, in its own crate, from calling
//! `SystemTime::now()` or opening a socket itself. What these cases establish
//! is the weaker and still useful claim: **the interface hands a strategy
//! nothing to reach with.** Enforcing the rest needs a lint or a review rule.

#[test]
fn a_strategy_cannot_reach_past_its_interface() {
    trybuild::TestCases::new().compile_fail("tests/compile-fail/*.rs");
}
