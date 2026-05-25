// Async task that drives the network stack forward.
//
// smoltcp doesn't run on its own — it expects somebody to call
// `Interface::poll()` periodically so it can ingest received Ethernet
// frames, run the TCP state machine, and emit outgoing packets. We do that
// here from a cooperative task that yields between polls so the rest of the
// kernel can keep running.

use crate::{browser, net, task::executor::yield_now};

pub async fn run() {
    loop {
        net::poll();
        // Refresh the browser status line / body once per poll iteration —
        // cheap, no-op when the browser isn't open.
        browser::tick();
        // Yield back to the executor. Other tasks get to run before we
        // re-poll; combined with the 100 Hz timer waker this keeps smoltcp
        // ticking at a healthy rate without burning CPU.
        yield_now().await;
    }
}
