use crossbeam::channel;
use crossbeam::channel::Sender;
use futures::task::{self, ArcWake};
use std::future::Future;
use std::pin::Pin;
use std::sync::atomic::AtomicPtr;
use std::sync::Arc;
use std::sync::Mutex;
use std::task::{Context, Poll};

pub mod time;

/// A structure holding a future and the result of
/// the latest call to its `poll` method.
struct TaskFuture<F>
where
    F: Future + Send + 'static,
{
    future: Pin<Box<F>>,
    poll: Poll<F::Output>,
}

impl<F> TaskFuture<F>
where
    F: Future + Send + 'static,
{
    fn new(future: F) -> TaskFuture<F> {
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

/// `#[repr(C)]` is necessary to ensure that the `Header`
/// field is always at the start of the struct. This
/// allows us to cast a pointer to the struct to a
/// pointer to the `Header` and vice versa.
#[repr(C)]
struct Task<F>
where
    F: Future + Send + 'static,
    F::Output: Send + 'static,
{
    _header: Header,
    // The `Mutex` is to make `Task` implement `Sync`. Only
    // one thread accesses `task_future` at any given time.
    // The `Mutex` is not required for correctness.
    task_future: Mutex<TaskFuture<F>>,
    executor: Sender<RawTask>,
}

impl<F> ArcWake for Task<F>
where
    F: Future + Send + 'static,
    F::Output: Send + 'static,
{
    fn wake_by_ref(arc_self: &Arc<Self>) {
        arc_self.schedule();
    }
}

impl<F> Task<F>
where
    F: Future + Send + 'static,
    F::Output: Send + 'static,
{
    fn into_raw(self: &Arc<Self>) -> RawTask {
        let raw = Arc::into_raw(self.clone());
        RawTask {
            ptr: AtomicPtr::new(raw.cast::<Header>() as *mut Header),
        }
    }

    fn schedule(self: &Arc<Self>) {
        let _ = self.executor.send(self.into_raw());
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
    fn spawn(future: F, sender: &channel::Sender<RawTask>) {
        let task = Arc::new(Task {
            _header: Header::new::<F>(),
            task_future: Mutex::new(TaskFuture::new(future)),
            executor: sender.clone(),
        });

        task.schedule();
    }
}

pub struct JoinHandle<T> {
    _phantom: std::marker::PhantomData<T>,
}

pub struct MiniTokio {
    scheduled: channel::Receiver<RawTask>,
    sender: channel::Sender<RawTask>,
}

impl MiniTokio {
    pub fn run(&self, num_threads: usize) {
        let mut workers = vec![];

        for _ in 0..num_threads {
            let scheduled = self.scheduled.clone();
            let worker = std::thread::spawn(move || {
                while let Ok(raw_task) = scheduled.recv() {
                    println!("Worker got a task!");
                    raw_task.poll();
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

        println!("All workers finished!");
    }

    pub fn new() -> MiniTokio {
        let (sender, scheduled) = channel::unbounded();
        MiniTokio { scheduled, sender }
    }

    /// Spawn a future onto the mini-tokio instance.
    ///
    /// The given future is wrapped with the `Task` harness and pushed into the
    /// `scheduled` queue. The future will be executed when `run` is called.
    pub fn spawn<F>(&self, future: F) -> JoinHandle<F::Output>
    where
        F: Future + Send + 'static,
        F::Output: Send + 'static,
    {
        Task::spawn(future, &self.sender);

        // The `JoinHandle` here is a placeholder for now.
        // This is simply a proof of concept that through
        // the use of type punning, we can create a `MiniTokio`
        // that can return a generic `JoinHandle<F::Output>`
        // while still being able to spawn a `Task<F>` that
        // can be polled by the `MiniTokio` instance.
        JoinHandle {
            _phantom: std::marker::PhantomData,
        }
    }
}

impl Default for MiniTokio {
    fn default() -> Self {
        Self::new()
    }
}

struct RawTask {
    ptr: AtomicPtr<Header>,
}

impl RawTask {
    fn poll(self) {
        let header = self.header();
        let vtable = header.vtable;
        (vtable.poll)(self.ptr)
    }

    /// Returns a reference to the task's header.
    fn header(&self) -> &Header {
        unsafe {
            self.ptr
                .load(std::sync::atomic::Ordering::Relaxed)
                .as_ref()
                .unwrap() // ptr should never be null
        }
    }
}

/// Header of a [`Task`] instance. `Header` is part of the magic that allows
/// [`Task`] to be generic type while `MiniTokio`, which is made up of [`Task`]s,
/// is not.
///
/// By casting between generically parameterized [`Task`]s and non-generic [`Header`]s, we
/// can store [`Task`]s of different types in the same data structure.
#[derive(Debug)]
struct Header {
    /// Table of function pointers for calling methods on a [`Task`].
    vtable: &'static Vtable,
}

impl Header {
    fn new<F: Future>() -> Header
    where
        F: Future + Send + 'static,
        F::Output: Send + 'static,
    {
        Header {
            vtable: vtable::<F>(),
        }
    }
}

#[derive(Debug)]
struct Vtable {
    /// Polls the future.
    poll: fn(AtomicPtr<Header>),
}

/// Wait, how is this allowed?! How can we return a static reference to a
/// struct that's created on the stack?!
///
/// This is possible because the
/// `Vtable` struct only contains a single field, which is a function pointer.
/// Function pointers are treated as &'static by the compiler, so the compiler
/// will allow us to return a &'static Vtable.
fn vtable<F: Future>() -> &'static Vtable
where
    F: Future + Send + 'static,
    F::Output: Send + 'static,
{
    &Vtable { poll: poll::<F> }
}

/// Casts an [`AtomicPtr<Header>`] back into the `[Arc<Task<F>>]` it came from,
/// and calls [`Task::poll`] on it.
fn poll<F>(ptr: AtomicPtr<Header>)
where
    F: Future + Send + 'static,
    F::Output: Send + 'static,
{
    // The pointer was created by `Arc::into_raw`, the task can (must) be re-created with `Arc::from_raw`.
    let task = unsafe { Arc::from_raw(ptr.into_inner() as *const Task<F>) };
    task.poll();
}
