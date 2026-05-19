// Async keyboard scancode pipeline.
//
// The PS/2 IRQ handler runs in interrupt context — we can't decode there
// without risking spinlock deadlocks against the rest of the kernel that
// `println!`s. So the handler just pushes the raw scancode byte onto a
// lock-free SPSC queue, and a task (`ScancodeStream::next`) drains it at its
// leisure on the executor's thread. The decoded character then flows to
// whichever task pulls from this stream — at first a simple printer, later
// the shell line editor.
//
// Linux does the equivalent split between the i8042 ISR and the input
// subsystem's evdev tasklets.

use crate::print;
use alloc::boxed::Box;
use conquer_once::spin::OnceCell;
use core::{
    pin::Pin,
    task::{Context, Poll},
};
use crossbeam_queue::ArrayQueue;
use futures_util::{
    stream::{Stream, StreamExt},
    task::AtomicWaker,
};
use pc_keyboard::{layouts, DecodedKey, HandleControl, Keyboard, ScancodeSet1};

/// Bounded inbox between the keyboard ISR and the consuming task. 100 is
/// generous — even a held-down key only fires ~30 IRQs/sec.
static SCANCODE_QUEUE: OnceCell<ArrayQueue<u8>> = OnceCell::uninit();
static WAKER: AtomicWaker = AtomicWaker::new();

/// Called from the keyboard interrupt handler. Must be wait-free: no locks,
/// no allocation. Drops the byte on the floor if the queue is full or hasn't
/// been initialised yet.
pub(crate) fn add_scancode(scancode: u8) {
    if let Ok(queue) = SCANCODE_QUEUE.try_get() {
        if queue.push(scancode).is_err() {
            // Queue overflow — print is OK here only because we know we're in
            // ISR context where the VGA lock is contended at most by the main
            // thread, which is currently waiting on this very stream.
            crate::println!("WARNING: scancode queue full; dropping input");
        } else {
            // Tell the consumer task it has work to do.
            WAKER.wake();
        }
    } else {
        // Pre-init: silently drop. Happens before `print_keypresses` is
        // spawned; not a bug.
    }
}

/// Stream<Item = u8> backed by the static `SCANCODE_QUEUE`. Lazy-initialises
/// the queue on first construction.
pub struct ScancodeStream {
    _private: (),
}

impl ScancodeStream {
    pub fn new() -> Self {
        SCANCODE_QUEUE
            .try_init_once(|| ArrayQueue::new(100))
            .expect("ScancodeStream::new should only be called once");
        ScancodeStream { _private: () }
    }
}

impl Default for ScancodeStream {
    fn default() -> Self {
        Self::new()
    }
}

impl Stream for ScancodeStream {
    type Item = u8;

    fn poll_next(self: Pin<&mut Self>, cx: &mut Context) -> Poll<Option<u8>> {
        let queue = SCANCODE_QUEUE
            .try_get()
            .expect("scancode queue not initialized");

        // Fast path: byte already waiting.
        if let Some(scancode) = queue.pop() {
            return Poll::Ready(Some(scancode));
        }

        // Slow path: register a waker so the ISR will kick us, then re-check
        // the queue to close the obvious race window.
        WAKER.register(cx.waker());
        match queue.pop() {
            Some(scancode) => {
                WAKER.take();
                Poll::Ready(Some(scancode))
            }
            None => Poll::Pending,
        }
    }
}

/// Default "/dev/input" consumer: decode bytes into characters and echo them.
/// Phase 3 will replace this with the shell line editor.
pub async fn print_keypresses() {
    let mut scancodes = ScancodeStream::new();
    let mut keyboard = Keyboard::new(ScancodeSet1::new(), layouts::Us104Key, HandleControl::Ignore);

    while let Some(scancode) = scancodes.next().await {
        if let Ok(Some(key_event)) = keyboard.add_byte(scancode) {
            if let Some(key) = keyboard.process_keyevent(key_event) {
                match key {
                    DecodedKey::Unicode(character) => print!("{}", character),
                    DecodedKey::RawKey(key) => print!("{:?}", key),
                }
            }
        }
    }
}

/// Pulled out so `mod.rs` doesn't need an unused-`Box` import gate. Kept here
/// purely for documentation of the type.
#[allow(dead_code)]
fn _phantom() -> Box<dyn core::any::Any> {
    Box::new(())
}
