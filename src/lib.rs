use crossbeam::channel;
use join_handle::JoinHandle;
use raw_task::RawTask;
use std::future::Future;
use task::Task;

pub mod join_handle;
mod raw_task;
mod task;
pub mod time;

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
                    unsafe { raw_task.poll() }
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
        Task::spawn(future, &self.sender)
    }
}

impl Default for MiniTokio {
    fn default() -> Self {
        Self::new()
    }
}
