use crossbeam::channel;
use crossbeam::channel::Sender;
use futures::task::{self, ArcWake};
use std::future::Future;
use std::mem;
use std::pin::Pin;
use std::sync::atomic::AtomicPtr;
use std::sync::Arc;
use std::sync::Mutex;
use std::task::{Context, Poll};

use crate::join_handle::JoinHandle;
use crate::raw_task::Header;
use crate::raw_task::RawTask;

enum TaskState<F>
where
    F: Future + Send + 'static,
    F::Output: Send + 'static,
{
    Pending(Box<F>),
    Completed(F::Output),
    Consumed,
}

impl<F> TaskState<F>
where
    F: Future + Send + 'static,
    F::Output: Send + 'static,
{
    fn new(future: F) -> TaskState<F> {
        TaskState::Pending(Box::new(future))
    }

    fn poll(&mut self, cx: &mut Context<'_>) {
        // Spurious wakeups are allowed, so we need to
        // check that the future is still pending before
        // calling `poll`. Failure to do so can lead to
        // a panic.
        match self {
            TaskState::Pending(ref mut future) => {
                // Safety: &mut self is mutually exclusive,
                // ergo we know that `self.future` is not
                // moved or dropped in this function.
                let pinned = unsafe { Pin::new_unchecked(future.as_mut()) };
                let poll = pinned.poll(cx);
                if poll.is_ready() {
                    match poll {
                        Poll::Ready(value) => {
                            *self = TaskState::Completed(value);
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

    fn try_read_output(&mut self, dst: &mut Poll<F::Output>, cx: &mut Context<'_>) {
        match self {
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
                        *dst = Poll::Ready(res);
                        *self = TaskState::Consumed;
                    }
                    Poll::Pending => {
                        *dst = Poll::Pending;
                    }
                }
            }
            TaskState::Completed(_) => match mem::replace(&mut *self, TaskState::Consumed) {
                TaskState::Completed(res) => {
                    *dst = Poll::Ready(res);
                }
                TaskState::Consumed | TaskState::Pending(_) => unreachable!(),
            },
            TaskState::Consumed => {
                panic!("tried to read output from a consumed task");
            }
        }
    }
}

/// `#[repr(C)]` is necessary to ensure that the `Header`
/// field is always at the start of the struct. This
/// allows us to cast a pointer to the struct to a
/// pointer to the `Header` and vice versa.
#[repr(C)]
pub struct Task<F>
where
    F: Future + Send + 'static,
    F::Output: Send + 'static,
{
    _header: Header,
    state: Mutex<TaskState<F>>,
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
    fn new(future: F, executor: &channel::Sender<RawTask>) -> Arc<Task<F>> {
        Arc::new(Task {
            _header: Header::new::<F>(),
            state: Mutex::new(TaskState::new(future)),
            executor: executor.clone(),
        })
    }

    fn schedule(self: &Arc<Self>) {
        let _ = self.executor.send(self.into_raw());
    }

    // Spawns a new task with the given future.
    //
    // Initializes a new Task harness containing the given future and pushes it
    // onto `sender`. The receiver half of the channel will get the task and
    // execute it.
    pub fn spawn(future: F, sender: &channel::Sender<RawTask>) -> JoinHandle<F> {
        let task = Task::new(future, sender);
        task.schedule();
        JoinHandle::new(task)
    }

    /// Polls the task's future.
    pub unsafe fn poll(self: Arc<Self>) {
        let mut task_future = self.state.lock().unwrap();

        // Create a waker from the `Task` instance. This
        // uses the `ArcWake` impl from above.
        let waker = task::waker(self.clone());
        let cx = &mut Context::from_waker(&waker);

        task_future.poll(cx)
    }

    /// Tries to read the output of the task's future.
    pub fn try_read_output(self: &Arc<Self>, dst: &mut Poll<F::Output>, cx: &mut Context<'_>) {
        let mut task_future = self.state.lock().unwrap();
        task_future.try_read_output(dst, cx);
    }

    /// Convert the [`Task`] to a [`RawTask`].
    ///
    /// To avoid a memory leak the [`RawTask`] must be converted back to a [`Task`] using [`Task::from_raw`].
    pub fn into_raw(self: &Arc<Self>) -> RawTask {
        let raw = Arc::into_raw(self.clone());
        // Destroy generic type by casting to a raw pointer to the header.
        RawTask::new(AtomicPtr::new(raw.cast::<Header>() as *mut Header))
    }

    /// Constructs a [`Task`] from a raw pointer to a [`Header`].
    ///
    /// This is used to convert a [`RawTask`] back to a [`Task`].
    ///
    /// The user of `from_raw` has to make sure a specific value of [`Task`] is only
    /// dropped once.
    ///
    /// This function is unsafe because improper use may lead to memory unsafety,
    /// even if the returned `Arc<Task<F>>` is never accessed.
    pub unsafe fn from_raw(ptr: &AtomicPtr<Header>) -> Arc<Task<F>> {
        Arc::from_raw(ptr.load(std::sync::atomic::Ordering::Relaxed) as *const Task<F>)
    }
}
