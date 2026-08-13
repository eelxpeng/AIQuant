// TEST-006 — INV-003: no `Add` across two value types.
// Positive control: TEST-002, where `Px + Px` works and is exact.

use types::{Px, Qty};

fn main() {
    let px = Px::from_scaled(123_450_000_000);
    let qty = Qty::from_scaled(1_000_000_000);

    let _ = px + qty;
    let _ = qty + px;
}
