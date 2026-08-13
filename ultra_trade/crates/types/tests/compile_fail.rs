//! TEST-006 … TEST-010 and TEST-017 … TEST-020 — the nine operations that must
//! not compile.
//!
//! The `.stderr` files are checked in on purpose. A compile-fail case passes if
//! the code fails to build for *any* reason, so without the expected message a
//! case could start passing because of a typo that fails to resolve a name —
//! which proves nothing about the type discipline.
//!
//! Each case has a positive control elsewhere in the suite; see
//! `specs/006-value-types/contracts/illegal-operations.md`.
//!
//! Regenerate after a toolchain bump:
//!
//! ```sh
//! TRYBUILD=overwrite cargo test -p types --test compile_fail
//! ```

#[test]
fn illegal_operations_do_not_compile() {
    let t = trybuild::TestCases::new();
    t.compile_fail("tests/compile-fail/*.rs");
}
