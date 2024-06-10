use crossbeam::channel;
use crossbeam::channel::Sender;
use futures::task::{self, ArcWake};
use std::future::Future;
use std::marker::PhantomData;
use std::mem;
use std::pin::Pin;
use std::sync::atomic::AtomicPtr;
use std::sync::Arc;
use std::sync::Mutex;
use std::task::{Context, Poll};

pub mod time;

enum TaskState<F>
where
    F: Future + Send + 'static,
    F::Output: Send + 'static,
{
    Pending(Box<F>),
    Completed(F::Output),
    Consumed,
}

/// A structure holding a future and the result of
/// the latest call to its `poll` method.
struct TaskFuture<F>
where
    F: Future + Send + 'static,
    F::Output: Send + 'static,
{
    state: TaskState<F>,
}

impl<F> TaskFuture<F>
where
    F: Future + Send + 'static,
    F::Output: Send + 'static,
{
    fn new(future: F) -> TaskFuture<F> {
        TaskFuture {
            state: TaskState::Pending(Box::new(future)),
        }
    }

    fn poll(&mut self, cx: &mut Context<'_>) {
        // Spurious wakeups are allowed, so we need to
        // check that the future is still pending before
        // calling `poll`. Failure to do so can lead to
        // a panic.
        match self.state {
            TaskState::Pending(ref mut future) => {
                // Safety: &mut self is mutually exclusive,
                // ergo we know that `self.future` is not
                // moved or dropped in this function.
                let pinned = unsafe { Pin::new_unchecked(future.as_mut()) };
                let poll = pinned.poll(cx);
                if poll.is_ready() {
                    match poll {
                        Poll::Ready(value) => {
                            self.state = TaskState::Completed(value);
                        }
                        Poll::Pending => unreachable!(),
                    }
                }
            }
            TaskState::Completed(_) => {
                println!("polled a completed task");
            }
            TaskState::Consumed => {
                println!("polled a consumed task");
            }
        }
    }

    fn try_read_output(&mut self, dst: &mut Poll<JoinResult<F::Output>>, cx: &mut Context<'_>) {
        match self.state {
            TaskState::Pending(ref mut future) => {
                // Safety: &mut self is mutually exclusive,
                // ergo we know that `self.future` is not
                // moved or dropped in this function.
                let pinned = unsafe { Pin::new_unchecked(future.as_mut()) };
                // This works nicely, because this function is only ever called when a Task is awaiting on
                // a JoinHandle, which has a [`RawTask`] as a member and which calls `try_read_output` on
                // that member.
                //
                // That means that here, we have the [`Context`] of the Task awaiting the JoinHandle, which
                // we're now passing to the `poll` of a Future owned by another Task. Therefore from here on
                // out, this Task awaiting the JoinHandle will have effectively taken ownership of that Future
                // from the original Task. When that future signals it's ready, the JoinHandle's Task (the
                // one that owns the `Context`) will be notified and the future will be polled again in this
                // code path.
                let poll = pinned.poll(cx);
                match poll {
                    Poll::Ready(res) => {
                        *dst = Poll::Ready(Ok(res));
                        self.state = TaskState::Consumed;
                    }
                    Poll::Pending => {
                        *dst = Poll::Pending;
                    }
                }
            }
            TaskState::Completed(_) => match mem::replace(&mut self.state, TaskState::Consumed) {
                TaskState::Completed(res) => {
                    *dst = Poll::Ready(Ok(res));
                }
                TaskState::Consumed | TaskState::Pending(_) => unreachable!(),
            },
            TaskState::Consumed => {
                panic!("polled a consumed task");
            }
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
    fn try_read_output(
        self: &Arc<Self>,
        dst: &mut Poll<JoinResult<F::Output>>,
        cx: &mut Context<'_>,
    ) {
        let mut task_future = self.task_future.lock().unwrap();
        task_future.try_read_output(dst, cx);
    }

    fn into_raw(self: &Arc<Self>) -> RawTask {
        let raw = Arc::into_raw(self.clone());
        RawTask {
            // Destroy generic type by casting to a raw pointer to the header.
            ptr: AtomicPtr::new(raw.cast::<Header>() as *mut Header),
        }
    }

    fn from_raw(ptr: &AtomicPtr<Header>) -> Arc<Task<F>> {
        unsafe { Arc::from_raw(ptr.load(std::sync::atomic::Ordering::Relaxed) as *const Task<F>) }
    }

    fn schedule(self: &Arc<Self>) {
        let _ = self.executor.send(self.into_raw());
    }

    fn poll(self: Arc<Self>) {
        let mut task_future = self.task_future.lock().unwrap();

        // Create a waker from the `Task` instance. This
        // uses the `ArcWake` impl from above.
        let waker = task::waker(self.clone());
        let cx = &mut Context::from_waker(&waker);
        task_future.poll(cx)
    }

    // Spawns a new task with the given future.
    //
    // Initializes a new Task harness containing the given future and pushes it
    // onto `sender`. The receiver half of the channel will get the task and
    // execute it.
    fn spawn(future: F, sender: &channel::Sender<RawTask>) -> RawTask {
        let task = Arc::new(Task {
            _header: Header::new::<F>(),
            task_future: Mutex::new(TaskFuture::new(future)),
            executor: sender.clone(),
        });

        task.schedule();

        task.into_raw()
    }
}

pub struct JoinHandle<F>
where
    F: Future + Send + 'static,
    F::Output: Send + 'static,
{
    raw: RawTask,
    _p: PhantomData<F>,
}

impl<F> Unpin for JoinHandle<F>
where
    F: Future + Send + 'static,
    F::Output: Send + 'static,
{
}

impl<F> JoinHandle<F>
where
    F: Future + Send + 'static,
    F::Output: Send + 'static,
{
    fn new(raw: RawTask) -> JoinHandle<F> {
        JoinHandle {
            raw,
            _p: PhantomData::<F>,
        }
    }
}

/// We must explicitly drop the [`RawTask`] when the [`JoinHandle`] is dropped,
/// because the [`RawTask`] is not dropped when the [`JoinHandle`] is dropped.
impl<F> Drop for JoinHandle<F>
where
    F: Future + Send + 'static,
    F::Output: Send + 'static,
{
    fn drop(&mut self) {
        unsafe {
            self.raw.drop();
        }
    }
}

impl<F> Future for JoinHandle<F>
where
    F: Future + Send + 'static,
    F::Output: Send + 'static,
{
    type Output = JoinResult<F::Output>;

    fn poll(mut self: Pin<&mut Self>, cx: &mut Context<'_>) -> Poll<Self::Output> {
        let mut ret = Poll::Pending;
        // Try to read the task output. If the task is not yet complete, the
        // cx is passed to the Future the task is waiting on, and thus it's waker
        // is notified when the task is complete.
        //
        // The function must go via the vtable, which requires erasing generic
        // types. To do this, the function "return" is placed on the stack
        // **before** calling the function and is passed into the function using
        // `*mut ()`.
        //
        // Safety:
        //
        // The type of `T` must match the task's output type.
        unsafe {
            self.raw = self.raw.try_read_output(&mut ret as *mut _ as *mut (), cx);
        }

        ret
    }
}

#[derive(Clone)]
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
    pub fn spawn<F>(&self, future: F) -> JoinHandle<F>
    where
        F: Future + Send + 'static,
        F::Output: Send + 'static,
    {
        let raw = Task::spawn(future, &self.sender);

        // The `JoinHandle` here is a placeholder for now.
        // This is simply a proof of concept that through
        // the use of type punning, we can create a `MiniTokio`
        // that can return a generic `JoinHandle<F::Output>`
        // while still being able to spawn a `Task<F>` that
        // can be polled by the `MiniTokio` instance.
        JoinHandle::new(raw)
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
    /// Safety: `dst` must be a `*mut Poll<super::Result<T::Output>>` where `T`
    /// is the future stored by the task.
    ///
    /// Returns a must_use [`RawTask`] because this is only called by JoinHandle,
    /// and we want JoinHandle to own the [`RawTask`] so that it can be dropped
    /// when the [`JoinHandle`] is dropped.
    #[must_use]
    unsafe fn try_read_output(&self, dst: *mut (), cx: &mut Context<'_>) -> Self {
        let vtable = self.header().vtable;
        (vtable.try_read_output)(&self.ptr, dst, cx)
    }

    fn poll(self) {
        let header = self.header();
        let vtable = header.vtable;
        (vtable.poll)(&self.ptr)
    }

    fn as_task<F>(&self) -> Arc<Task<F>>
    where
        F: Future + Send + 'static,
        F::Output: Send + 'static,
    {
        Task::<F>::from_raw(&self.ptr)
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

    unsafe fn drop(&mut self) {
        let header = self.header();
        let vtable = header.vtable;
        (vtable.drop)(&self.ptr);
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
    poll: fn(&AtomicPtr<Header>),
    /// Reads the task output, if complete.
    try_read_output: unsafe fn(&AtomicPtr<Header>, *mut (), &mut Context<'_>) -> RawTask,
    /// Drops the task.
    drop: unsafe fn(&AtomicPtr<Header>),
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
    &Vtable {
        poll: poll::<F>,
        try_read_output: try_read_output::<F>,
        drop: drop::<F>,
    }
}

/// Casts an [`AtomicPtr<Header>`] back into the `[Arc<Task<F>>]` it came from,
/// and calls [`Task::poll`] on it.
fn poll<F>(ptr: &AtomicPtr<Header>)
where
    F: Future + Send + 'static,
    F::Output: Send + 'static,
{
    // The pointer was created by `Arc::into_raw`, the task can (must) be re-created with `Arc::from_raw`.
    let task = Task::<F>::from_raw(ptr);
    task.poll();
}

#[derive(Debug)]
pub struct JoinError(String);
pub type JoinResult<T> = Result<T, JoinError>;

unsafe fn try_read_output<F>(ptr: &AtomicPtr<Header>, dst: *mut (), cx: &mut Context<'_>) -> RawTask
where
    F: Future + Send + 'static,
    F::Output: Send + 'static,
{
    let out = &mut *(dst as *mut Poll<JoinResult<F::Output>>);
    let task = Task::<F>::from_raw(ptr);
    task.try_read_output(out, cx);
    // Don't decrease the reference count on the task here,
    // let JoinHandle manage that itself.
    task.into_raw()
}

/// # Safety
///
/// - `ptr` must be a valid pointer to a [`Task`].
unsafe fn drop<F>(ptr: &AtomicPtr<Header>)
where
    F: Future + Send + 'static,
    F::Output: Send + 'static,
{
    let _task = Task::<F>::from_raw(ptr);
}
