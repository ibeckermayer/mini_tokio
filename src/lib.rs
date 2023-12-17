use crossbeam::channel;
use crossbeam::channel::Sender;
use futures::task::{self, ArcWake};
use std::future::Future;
use std::pin::Pin;
use std::sync::Arc;
use std::sync::Mutex;
use std::task::{Context, Poll};

pub mod time;

/// A structure holding a future and the result of
/// the latest call to its `poll` method.
struct TaskFuture {
    future: Pin<Box<dyn Future<Output = ()> + Send>>,
    poll: Poll<()>,
}

impl TaskFuture {
    fn new(future: impl Future<Output = ()> + Send + 'static) -> TaskFuture {
        TaskFuture {
            future: Box::pin(future),
            poll: Poll::Pending,
        }
    }

    fn poll(&mut self, cx: &mut Context<'_>) {
        // Spurious wakeups are allowed, so we need to
        // check that the future is still pending before
        // calling `poll`. Failure to do so can lead to
        // a panic.
        if self.poll.is_pending() {
            self.poll = self.future.as_mut().poll(cx);
        }
    }
}

struct Task {
    // The `Mutex` is to make `Task` implement `Sync`. Only
    // one thread accesses `task_future` at any given time.
    // The `Mutex` is not required for correctness. Real Tokio
    // does not use a mutex here, but real Tokio has
    // more lines of code than can fit in a single tutorial
    // page.
    task_future: Mutex<TaskFuture>,
    executor: Sender<Arc<Task>>,
}

impl ArcWake for Task {
    fn wake_by_ref(arc_self: &Arc<Self>) {
        arc_self.schedule();
    }
}

impl Task {
    fn schedule(self: &Arc<Self>) {
        let _ = self.executor.send(self.clone());
    }

    fn poll(self: Arc<Self>) {
        // Create a waker from the `Task` instance. This
        // uses the `ArcWake` impl from above.
        let waker = task::waker(self.clone());
        let mut cx = Context::from_waker(&waker);

        // No other thread ever tries to lock the task_future
        let mut task_future = self.task_future.try_lock().unwrap();

        // Poll the inner future
        task_future.poll(&mut cx);
    }

    // Spawns a new task with the given future.
    //
    // Initializes a new Task harness containing the given future and pushes it
    // onto `sender`. The receiver half of the channel will get the task and
    // execute it.
    fn spawn<F>(future: F, sender: &channel::Sender<Arc<Task>>)
    where
        F: Future<Output = ()> + Send + 'static,
    {
        let task = Arc::new(Task {
            task_future: Mutex::new(TaskFuture::new(future)),
            executor: sender.clone(),
        });

        // TODO: this function should return a result
        // which indicates whether the task was
        // successfully scheduled or not.
        let _ = sender.send(task);
    }
}

pub struct MiniTokio {
    scheduled: channel::Receiver<Arc<Task>>,
    sender: channel::Sender<Arc<Task>>,
}

impl MiniTokio {
    pub fn run(&self, num_threads: usize) {
        let mut workers = vec![];

        for _ in 0..num_threads {
            let scheduled = self.scheduled.clone();
            let worker = std::thread::spawn(move || {
                while let Ok(task) = scheduled.recv() {
                    task.poll();
                }
            });
            workers.push(worker);
        }

        // Wait for all workers to finish
        for worker in workers {
            if let Err(e) = worker.join() {
                println!("Error joining worker thread: {:?}", e);
            }
        }
    }

    pub fn new() -> MiniTokio {
        let (sender, scheduled) = channel::unbounded();
        MiniTokio { scheduled, sender }
    }

    /// Spawn a future onto the mini-tokio instance.
    ///
    /// The given future is wrapped with the `Task` harness and pushed into the
    /// `scheduled` queue. The future will be executed when `run` is called.
    pub fn spawn<F>(&self, future: F)
    where
        F: Future<Output = ()> + Send + 'static,
    {
        Task::spawn(future, &self.sender);
    }
}

impl Default for MiniTokio {
    fn default() -> Self {
        Self::new()
    }
}
