//! Bounded preparation shared by retained cells, live revisions and invocation panes.

use crate::tui::FrameRequester;
use std::sync::Arc;
use std::sync::OnceLock;
use std::sync::Weak;
use std::sync::atomic::{AtomicUsize, Ordering};
use std::thread::JoinHandle;
use std::time::Duration;
use tokio::sync::mpsc::{self, Sender, error::TrySendError};
use tokio::sync::oneshot::{self, Receiver, error::TryRecvError};

struct Job {
    owner: Option<Weak<()>>,
    render: Box<dyn FnOnce() + Send>,
    frames: FrameRequester,
}

struct Pool {
    lanes: Vec<Sender<Job>>,
    next: AtomicUsize,
    _threads: Vec<JoinHandle<()>>,
}

impl Pool {
    #[expect(
        clippy::expect_used,
        reason = "The shared preparation pool cannot initialize without its mandatory worker threads."
    )]
    fn get() -> &'static Self {
        static POOL: OnceLock<Pool> = OnceLock::new();
        POOL.get_or_init(|| {
            let mut lanes = Vec::new();
            let mut threads = Vec::new();
            for index in 0..2 {
                let (requests, mut incoming) = mpsc::channel::<Job>(16);
                lanes.push(requests);
                threads.push(
                    std::thread::Builder::new()
                        .name(format!("transcript-preparation-{index}"))
                        .spawn(move || {
                            while let Some(job) = incoming.blocking_recv() {
                                if job
                                    .owner
                                    .as_ref()
                                    .is_none_or(|owner| owner.strong_count() != 0)
                                {
                                    (job.render)();
                                    job.frames.schedule_frame();
                                }
                            }
                        })
                        .expect("start transcript preparation worker"),
                );
            }
            Self {
                lanes,
                next: AtomicUsize::new(0),
                _threads: threads,
            }
        })
    }
    fn lane(&self, frames: FrameRequester) -> Lane {
        let index = self.next.fetch_add(1, Ordering::Relaxed) % self.lanes.len();
        Lane {
            requests: self.lanes[index].clone(),
            frames,
        }
    }
}

/// A stream's parser and syntax highlighter remain on one worker for their entire lifetime.
#[derive(Clone)]
pub(crate) struct Lane {
    requests: Sender<Job>,
    frames: FrameRequester,
}
impl Lane {
    pub(crate) fn new(frames: FrameRequester) -> Self {
        Pool::get().lane(frames)
    }
    pub(crate) fn spawn(&self, render: impl FnOnce() + Send + 'static) {
        let job = Job {
            owner: None,
            render: Box::new(render),
            frames: self.frames.clone(),
        };
        if let Err(error) = self.requests.try_send(job) {
            match error {
                TrySendError::Full(job) => {
                    let requests = self.requests.clone();
                    tokio::spawn(async move {
                        requests
                            .send(job)
                            .await
                            .unwrap_or_else(|_| panic!("transcript preparation pool stopped"));
                    });
                }
                TrySendError::Closed(_) => panic!("transcript preparation pool stopped"),
            }
        }
    }
}

pub(crate) struct Pending<T> {
    queued: Option<Job>,
    result: Receiver<T>,
    requests: Sender<Job>,
    owner: Arc<()>,
}

impl<T: Send + 'static> Pending<T> {
    pub(crate) fn new(frames: FrameRequester, render: impl FnOnce() -> T + Send + 'static) -> Self {
        Self::on_lane(&Lane::new(frames), render)
    }
    pub(crate) fn on_lane(lane: &Lane, render: impl FnOnce() -> T + Send + 'static) -> Self {
        let (result, receiver) = oneshot::channel();
        let owner = Arc::new(());
        Self {
            queued: Some(Job {
                owner: Some(Arc::downgrade(&owner)),
                frames: lane.frames.clone(),
                render: Box::new(move || {
                    let _ = result.send(render());
                }),
            }),
            requests: lane.requests.clone(),
            result: receiver,
            owner,
        }
    }

    pub(crate) fn poll(&mut self) -> Option<T> {
        self.submit();
        match self.result.try_recv() {
            Ok(result) => Some(result),
            Err(TryRecvError::Empty) => None,
            Err(TryRecvError::Closed) => panic!("transcript preparation worker stopped"),
        }
    }

    pub(crate) fn submit(&mut self) {
        if let Some(job) = self.queued.take() {
            match self.requests.try_send(job) {
                Ok(()) => {}
                Err(TrySendError::Full(job)) => {
                    job.frames.schedule_content_in(Duration::from_millis(10));
                    self.queued = Some(job);
                }
                Err(TrySendError::Closed(_)) => panic!("transcript preparation pool stopped"),
            }
        }
    }

    #[expect(
        clippy::expect_used,
        reason = "The live task retains its cancellation owner until its worker returns a result."
    )]
    pub(crate) async fn wait(mut self) -> T {
        if let Some(job) = self.queued.take() {
            self.requests
                .send(job)
                .await
                .unwrap_or_else(|_| panic!("transcript preparation pool stopped"));
        }
        let result = self
            .result
            .await
            .expect("transcript preparation worker stopped");
        drop(self.owner);
        result
    }
}
