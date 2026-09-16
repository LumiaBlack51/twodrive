type ReadJob = Box<dyn FnOnce() + Send>;
use std::sync::mpsc::{self, Sender};
use std::sync::{Arc, Mutex};
use std::thread::{self, JoinHandle};

pub(crate) struct ReadPool {
    pub(crate) sender: Option<Sender<ReadJob>>,
    pub(crate) workers: Vec<JoinHandle<()>>,
}

impl ReadPool {
    pub(crate) fn new() -> Self {
        let (sender, receiver) = mpsc::channel::<ReadJob>();
        let receiver = Arc::new(Mutex::new(receiver));
        let workers = (0..4)
            .map(|_| {
                let receiver = Arc::clone(&receiver);
                thread::spawn(move || {
                    loop {
                        let job = match receiver.lock() {
                            Ok(receiver) => receiver.recv(),
                            Err(_) => return,
                        };
                        match job {
                            Ok(job) => job(),
                            Err(_) => return,
                        }
                    }
                })
            })
            .collect();
        Self {
            sender: Some(sender),
            workers,
        }
    }

    pub(crate) fn spawn(&self, job: impl FnOnce() + Send + 'static) {
        if let Some(sender) = &self.sender {
            let _ = sender.send(Box::new(job));
        }
    }
}

impl Drop for ReadPool {
    fn drop(&mut self) {
        self.sender.take();
        for worker in self.workers.drain(..) {
            let _ = worker.join();
        }
    }
}
