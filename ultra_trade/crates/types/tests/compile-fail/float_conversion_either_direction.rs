// TEST-009 — INV-002: no conversion between a value type and a float, in
// either direction (Constitution VII).
// Positive control: TEST-001, where `from_scaled`/`to_scaled` is the only
// conversion and it round trips exactly.

use types::{Notional, Px, Qty};

fn main() {
    let px = Px::from_scaled(123_450_000_000);

    let _ = Px::from(123.45_f64);
    let _ = Qty::from(1.5_f32);
    let _: f64 = px.into();
    let _ = px as f64;
    let _: f64 = Notional::from_scaled(1).into();
}
