// TEST-008 — FR-006: a bare operator has nowhere to name a rounding direction,
// and an exact product can need more decimal places than the scale holds.
// Positive control: TEST-003 / TEST-004, where `px.notional(qty, dir)` builds
// and yields a `Notional`.

use types::{Px, Qty};

fn main() {
    let px = Px::from_scaled(10_000_100_000);
    let qty = Qty::from_scaled(1_000);

    let _ = px * qty;
    let _ = qty * px;
}
