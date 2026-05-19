// Single-threaded cooperative executor.
//
// Tasks live in a `BTreeMap<TaskId, Task>`. A second map of `Waker`s lets us
// avoid recreating one per poll. A bounded `ArrayQueue<TaskId>` tells us which
// tasks are ready to run; `wake_by_ref` pushes the id of the woken task onto
// the queue and `run_ready_tasks` drains it. When the queue is empty we
// `hlt` until the next interrupt, instead of busy-looping.

use super::{Task, TaskId};
use alloc::{collections::BTreeMap, sync::Arc, task::Wake};
use core::task::{Context, Poll, Waker};
use crossbeam_queue::ArrayQueue;

/// Default room for runnable tasks. Bounded queue avoids unbounded growth if
/// some buggy task wakes itself forever. 100 is plenty for kernel work.
const TASK_QUEUE_CAPACITY: usize = 100;

pub struct Executor {
    tasks: BTreeMap<TaskId, Task>,
    task_queue: Arc<ArrayQueue<TaskId>>,
    waker_cache: BTreeMap<TaskId, Waker>,
}

impl Executor {
    pub fn new() -> Self {
        Executor {
            tasks: BTreeMap::new(),
            task_queue: Arc::new(ArrayQueue::new(TASK_QUEUE_CAPACITY)),
            waker_cache: BTreeMap::new(),
        }
    }

    /// Add a task to the executor. The new task is immediately enqueued so the
    /// next call to `run_ready_tasks` will poll it once.
    pub fn spawn(&mut self, task: Task) {
        let task_id = task.id;
        if self.tasks.insert(task.id, task).is_some() {
            panic!("task with same ID already in tasks");
        }
        self.task_queue.push(task_id).expect("queue full");
    }

    /// Drain the run queue, polling each task once.
    fn run_ready_tasks(&mut self) {
        let Self {
            tasks,
            task_queue,
            waker_cache,
        } = self;

        while let Some(task_id) = task_queue.pop() {
            let task = match tasks.get_mut(&task_id) {
                Some(task) => task,
                None => continue, // task no longer exists
            };
            let waker = waker_cache
                .entry(task_id)
                .or_insert_with(|| TaskWaker::new(task_id, task_queue.clone()));
            let mut context = Context::from_waker(waker);
            match task.poll(&mut context) {
                Poll::Ready(()) => {
                    tasks.remove(&task_id);
                    waker_cache.remove(&task_id);
                }
                Poll::Pending => {}
            }
        }
    }

    /// Race-free idle: disable interrupts, check if the queue is empty, hlt
    /// (which atomically re-enables interrupts). If we instead checked then
    /// hlt'd separately, an interrupt could fire between the two and we'd
    /// sleep forever.
    fn sleep_if_idle(&self) {
        use x86_64::instructions::interrupts::{self, enable_and_hlt};

        interrupts::disable();
        if self.task_queue.is_empty() {
            enable_and_hlt();
        } else {
            interrupts::enable();
        }
    }

    /// Top-level loop. Never returns — drives tasks to completion forever.
    pub fn run(&mut self) -> ! {
        loop {
            self.run_ready_tasks();
            self.sleep_if_idle();
        }
    }
}

impl Default for Executor {
    fn default() -> Self {
        Self::new()
    }
}

/// The waker an `Executor` hands a task. Cloning a waker is cheap (it's an
/// `Arc`) so each poll can stash one in `Context` without allocation.
struct TaskWaker {
    task_id: TaskId,
    task_queue: Arc<ArrayQueue<TaskId>>,
}

impl TaskWaker {
    fn new(task_id: TaskId, task_queue: Arc<ArrayQueue<TaskId>>) -> Waker {
        Waker::from(Arc::new(TaskWaker {
            task_id,
            task_queue,
        }))
    }

    fn wake_task(&self) {
        self.task_queue.push(self.task_id).expect("task_queue full");
    }
}

impl Wake for TaskWaker {
    fn wake(self: Arc<Self>) {
        self.wake_task();
    }

    fn wake_by_ref(self: &Arc<Self>) {
        self.wake_task();
    }
}
