use std::{
    marker::PhantomData,
    pin::Pin,
    sync::Arc,
    task::{Context, Poll},
};

use futures::Future;

use crate::{raw_task::RawTask, task::Task};

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
    pub fn new(task: Arc<Task<F>>) -> JoinHandle<F> {
        JoinHandle {
            raw: task.into_raw(),
            _p: PhantomData::<F>,
        }
    }
}

/// We must explicitly drop the [`RawTask`] when the [`JoinHandle`] is dropped,
/// because the [`JoinHandle`] owns it's [`RawTask`] and it's otherwise not
/// properly dropped when the [`JoinHandle`] is dropped.
impl<F> Drop for JoinHandle<F>
where
    F: Future + Send + 'static,
    F::Output: Send + 'static,
{
    fn drop(&mut self) {
        // Safety: We own a `RawTask` and thus can safely drop it manually.
        unsafe { self.raw.drop() }
    }
}

impl<F> Future for JoinHandle<F>
where
    F: Future + Send + 'static,
    F::Output: Send + 'static,
{
    type Output = F::Output;

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
