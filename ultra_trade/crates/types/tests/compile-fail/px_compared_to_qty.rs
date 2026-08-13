// TEST-007 — INV-003: no ordering or equality across two value types.
// Comparison is a separate guard from addition; a type can reject one and
// accept the other.
// Positive control: TEST-002, where `Px < Px` matches integer ordering.

use types::{Px, Qty};

fn main() {
    let px = Px::from_scaled(123_450_000_000);
    let qty = Qty::from_scaled(1_000_000_000);

    let _ = px < qty;
    let _ = px == qty;
}
