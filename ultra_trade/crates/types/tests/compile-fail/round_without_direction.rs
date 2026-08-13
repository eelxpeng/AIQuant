// TEST-010 — INV-005: there is no default rounding direction and no
// single-argument rounding entry point.
// Positive control: TEST-014, where the two-argument call works across the
// whole direction table.

use types::{Px, Qty, SCALE};

fn main() {
    let px = Px::from_scaled(10_004_000_000);
    let tick = Px::from_scaled(10_000_000);
    let qty = Qty::from_scaled(1_500_000_000);
    let lot = Qty::from_scaled(SCALE);

    let _ = px.round_to_tick(tick);
    let _ = qty.round_to_lot(lot);
}
