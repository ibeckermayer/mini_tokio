use std::future::Future;
use std::sync::atomic::AtomicPtr;
use std::task::{Context, Poll};

use crate::task::Task;

pub struct RawTask {
    ptr: AtomicPtr<Header>,
}

impl RawTask {
    pub fn new(ptr: AtomicPtr<Header>) -> RawTask {
        RawTask { ptr }
    }

    /// Safety: `dst` must be a `*mut Poll<super::Result<T::Output>>` where `T`
    /// is the future stored by the task.
    ///
    /// Returns a must_use [`RawTask`] because this is only called by JoinHandle,
    /// and we want JoinHandle to own the [`RawTask`] so that it can be dropped
    /// when the [`JoinHandle`] is dropped.
    #[must_use]
    pub unsafe fn try_read_output(&self, dst: *mut (), cx: &mut Context<'_>) -> Self {
        let vtable = self.header().vtable;
        (vtable.try_read_output)(&self.ptr, dst, cx)
    }

    pub unsafe fn poll(self) {
        let header = self.header();
        let vtable = header.vtable;
        (vtable.poll)(&self.ptr)
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

    pub unsafe fn drop(&mut self) {
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
pub struct Header {
    /// Table of function pointers for calling methods on a [`Task`].
    vtable: &'static Vtable,
}

impl Header {
    /// Creates a new [`Header`] for a [`Task`] of type `F`.
    pub fn new<F: Future>() -> Header
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
    poll: unsafe fn(&AtomicPtr<Header>),
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
///
/// # Safety
///
/// - `ptr` must be a valid pointer to a [`Task`].
unsafe fn poll<F>(ptr: &AtomicPtr<Header>)
where
    F: Future + Send + 'static,
    F::Output: Send + 'static,
{
    // The pointer was created by `Arc::into_raw`, the task can (must) be re-created with `Arc::from_raw`.
    let task = Task::<F>::from_raw(ptr);
    task.poll();
}

/// # Safety
///
/// - `ptr` must be a valid pointer to a [`Task`].
unsafe fn try_read_output<F>(ptr: &AtomicPtr<Header>, dst: *mut (), cx: &mut Context<'_>) -> RawTask
where
    F: Future + Send + 'static,
    F::Output: Send + 'static,
{
    let out = &mut *(dst as *mut Poll<F::Output>);
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
