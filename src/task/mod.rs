// Cooperative multitasking — Linux's `kernel/sched` analogue, but voluntary.
//
// We don't (yet) preempt anything; tasks are `Future`s that the executor
// `poll`s in a single thread of control. Whenever a task wants to wait — for
// keyboard input, a timer tick, whatever — it returns `Poll::Pending`, the
// executor parks it, and resumes it later when its `Waker` is signalled.
//
// This is the same model Linux uses for io_uring's `IORING_SETUP_COOP_TASKRUN`
// and how all the modern async runtimes (Tokio, async-std) schedule work.

use alloc::boxed::Box;
use core::{
    future::Future,
    pin::Pin,
    sync::atomic::{AtomicU64, Ordering},
    task::{Context, Poll},
};

pub mod executor;
pub mod keyboard;
pub mod mouse;
pub mod tick;

/// Process-id–like handle for a task. Just a strictly increasing counter; we
/// use it to key the executor's task table and to wake by id.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub struct TaskId(u64);

impl TaskId {
    fn new() -> Self {
        static NEXT_ID: AtomicU64 = AtomicU64::new(0);
        TaskId(NEXT_ID.fetch_add(1, Ordering::Relaxed))
    }
}

/// A unit of asynchronous kernel work. Owns the future on the heap and pins
/// it so we can safely poll it from anywhere.
pub struct Task {
    id: TaskId,
    future: Pin<Box<dyn Future<Output = ()>>>,
}

impl Task {
    pub fn new(future: impl Future<Output = ()> + 'static) -> Task {
        Task {
            id: TaskId::new(),
            future: Box::pin(future),
        }
    }

    fn poll(&mut self, context: &mut Context) -> Poll<()> {
        self.future.as_mut().poll(context)
    }
}
