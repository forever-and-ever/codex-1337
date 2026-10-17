//! Bounded preparation shared by retained cells, live revisions and invocation panes.

use crate::tui::FrameRequester;
use std::sync::Arc;
use std::sync::Mutex;
use std::sync::OnceLock;
use std::sync::Weak;
use std::sync::mpsc::{self, Receiver, SyncSender, TryRecvError, TrySendError};
use std::thread::JoinHandle;
use std::time::Duration;

struct Job {
    owner: Weak<()>,
    render: Box<dyn FnOnce() + Send>,
    frames: FrameRequester,
}

struct Pool {
    requests: SyncSender<Job>,
    // Process-lifetime workers own only in-memory jobs; dropping a request cancels it.
    _threads: Vec<JoinHandle<()>>,
}

impl Pool {
    fn get() -> &'static Self {
        static POOL: OnceLock<Pool> = OnceLock::new();
        POOL.get_or_init(|| {
            let (requests, incoming) = mpsc::sync_channel::<Job>(32);
            let incoming = Arc::new(Mutex::new(incoming));
            let threads = (0..2)
                .map(|index| {
                    let incoming = Arc::clone(&incoming);
                    std::thread::Builder::new()
                        .name(format!("transcript-preparation-{index}"))
                        .spawn(move || {
                            loop {
                                let job =
                                    incoming.lock().expect("preparation queue poisoned").recv();
                                let Ok(job) = job else { break };
                                if job.owner.strong_count() != 0 {
                                    (job.render)();
                                    job.frames.schedule_frame();
                                }
                            }
                        })
                        .expect("start transcript preparation worker")
                })
                .collect();
            Pool {
                requests,
                _threads: threads,
            }
        })
    }
}

pub(super) struct Pending<T> {
    queued: Option<Job>,
    result: Receiver<T>,
    _owner: Arc<()>,
}

impl<T: Send + 'static> Pending<T> {
    pub(super) fn new(frames: FrameRequester, render: impl FnOnce() -> T + Send + 'static) -> Self {
        let (result, receiver) = mpsc::channel();
        let owner = Arc::new(());
        Self {
            queued: Some(Job {
                owner: Arc::downgrade(&owner),
                frames,
                render: Box::new(move || {
                    let _ = result.send(render());
                }),
            }),
            result: receiver,
            _owner: owner,
        }
    }

    pub(super) fn poll(&mut self) -> Option<T> {
        self.submit();
        match self.result.try_recv() {
            Ok(result) => Some(result),
            Err(TryRecvError::Empty) => None,
            Err(TryRecvError::Disconnected) => panic!("transcript preparation worker stopped"),
        }
    }

    pub(super) fn submit(&mut self) {
        if let Some(job) = self.queued.take() {
            match Pool::get().requests.try_send(job) {
                Ok(()) => {}
                Err(TrySendError::Full(job)) => {
                    job.frames.schedule_content_in(Duration::from_millis(10));
                    self.queued = Some(job);
                }
                Err(TrySendError::Disconnected(_)) => panic!("transcript preparation pool stopped"),
            }
        }
    }
}
