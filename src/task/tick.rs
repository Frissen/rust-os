// Async one-Hz tick stream — feeds the AiOC desktop clock task.
//
// The PIT fires at 100 Hz. The timer ISR calls `notify_tick()` on every
// IRQ; every 100th call we push a sentinel onto a lock-free queue and wake
// the consumer task. This mirrors the ISR↔task split we use for keyboard
// scancodes (see `task::keyboard`).

use conquer_once::spin::OnceCell;
use core::{
    pin::Pin,
    sync::atomic::{AtomicU32, Ordering},
    task::{Context, Poll},
};
use crossbeam_queue::ArrayQueue;
use futures_util::{stream::Stream, task::AtomicWaker};

/// Bounded inbox for "second has elapsed" notifications. A capacity of 8
/// lets the consumer task be a couple of seconds late without losing
/// updates outright; the queue saturates if the executor truly stalls,
/// which is fine — we're driving a wall clock, not a real-time signal.
static QUEUE: OnceCell<ArrayQueue<()>> = OnceCell::uninit();
static WAKER: AtomicWaker = AtomicWaker::new();

/// Sub-second counter incremented on every PIT IRQ. Wraps at 100.
static SUBTICK: AtomicU32 = AtomicU32::new(0);

/// Called from the timer ISR (`interrupts::timer_interrupt_handler`).
/// Wait-free: no locks, no allocation.
pub fn notify_tick() {
    let n = SUBTICK.fetch_add(1, Ordering::Relaxed) + 1;
    if n >= 100 {
        SUBTICK.store(0, Ordering::Relaxed);
        if let Ok(q) = QUEUE.try_get() {
            // Best-effort push: a full queue means the consumer is behind,
            // which is fine — the wall clock can skip a second.
            let _ = q.push(());
            WAKER.wake();
        }
    }
}

/// Stream that yields `()` once per second of wall-clock time.
pub struct SecondStream {
    _private: (),
}

impl SecondStream {
    pub fn new() -> Self {
        QUEUE
            .try_init_once(|| ArrayQueue::new(8))
            .expect("SecondStream::new should only be called once");
        SecondStream { _private: () }
    }
}

impl Default for SecondStream {
    fn default() -> Self {
        Self::new()
    }
}

impl Stream for SecondStream {
    type Item = ();

    fn poll_next(self: Pin<&mut Self>, cx: &mut Context) -> Poll<Option<()>> {
        let q = QUEUE.try_get().expect("tick queue not initialized");
        if q.pop().is_some() {
            return Poll::Ready(Some(()));
        }
        WAKER.register(cx.waker());
        match q.pop() {
            Some(_) => {
                WAKER.take();
                Poll::Ready(Some(()))
            }
            None => Poll::Pending,
        }
    }
}
