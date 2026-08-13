//! D3.4: only complete bars reach a strategy. There is no event carrying a bar
//! whose window is still open, so reading a close before it is final is not
//! something a strategy can express.

use marketdata::BarSubscription;
use strategy::StrategyEvent;

fn main() {
    // Legal: the complete-bar variant is the one that exists.
    fn accepts_an_event(_: StrategyEvent<'_>) {}
    let _ = accepts_an_event;
    let _ = BarSubscription::from_index(0);

    let _ = StrategyEvent::FormingBar {
        subscription: BarSubscription::from_index(0),
    };
}
