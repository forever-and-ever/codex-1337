//! A batch sample returns a handle before its first paid POST. Readers retrieve
//! the single captured fork result; they never start inference or execute tools.
use super::SAMPLE_TIMEOUT;
use super::SampleTask;
use super::collect_response;
use crate::error_code::internal_error;
use crate::error_code::invalid_request;
use codex_app_server_protocol::JSONRPCErrorError;
use codex_app_server_protocol::ThreadSampleReadParams;
use codex_app_server_protocol::ThreadSampleReadResponse;
use codex_app_server_protocol::ThreadSampleStartResponse;
use codex_core::CodexThread;
use codex_core::OpenRouterSampleStatus;
use codex_protocol::ThreadId;
use codex_protocol::openrouter::OpenRouterTransport;
use futures::StreamExt;
use serde::Deserialize;
use serde::Serialize;
use sha2::Digest;
use sha2::Sha256;
use std::fs::File;
use std::fs::OpenOptions;
use std::io::Write;
use std::os::unix::fs::DirBuilderExt;
use std::os::unix::fs::OpenOptionsExt;
use std::path::Path;
use std::path::PathBuf;
use std::sync::OnceLock;
use std::time::Duration;
use tokio::time::Instant;
use uuid::Uuid;

#[derive(Deserialize, Serialize)]
enum Capture {
    Starting(Uuid),
    Fork {
        fork: ThreadId,
        actor: Option<Uuid>,
        unchanged: Option<String>,
    },
    Failed(String),
}

#[derive(Deserialize, Serialize)]
struct Record {
    source_turn: String,
    capture: Capture,
}

fn boot() -> Uuid {
    static BOOT: OnceLock<Uuid> = OnceLock::new();
    *BOOT.get_or_init(Uuid::new_v4)
}

pub(crate) struct SampleHandle {
    path: PathBuf,
    source_turn: String,
}

impl SampleHandle {
    fn directory(home: &Path, owner: ThreadId) -> PathBuf {
        home.join("openrouter-samples").join(owner.to_string())
    }

    fn new(
        home: &Path,
        owner: ThreadId,
        id: Uuid,
        source_turn: String,
    ) -> Result<Self, JSONRPCErrorError> {
        let directory = Self::directory(home, owner);
        let err = |error: std::io::Error| internal_error(error.to_string());
        std::fs::DirBuilder::new()
            .recursive(true)
            .mode(0o700)
            .create(&directory)
            .map_err(err)?;
        let path = directory.join(id.to_string());
        let mut file = OpenOptions::new()
            .write(true)
            .create_new(true)
            .mode(0o600)
            .open(&path)
            .map_err(err)?;
        let record = Record {
            source_turn: source_turn.clone(),
            capture: Capture::Starting(boot()),
        };
        serde_json::to_writer(&mut file, &record).map_err(|e| internal_error(e.to_string()))?;
        file.flush()
            .and_then(|()| file.sync_all())
            .and_then(|()| codex_core::sync_openrouter_owner(home, &directory))
            .map_err(err)?;
        Ok(Self { path, source_turn })
    }

    fn save(&self, capture: Capture) -> Result<(), JSONRPCErrorError> {
        let record = Record {
            source_turn: self.source_turn.clone(),
            capture,
        };
        let dir = self
            .path
            .parent()
            .ok_or_else(|| internal_error("invalid sample path"))?;
        let mut tmp =
            tempfile::NamedTempFile::new_in(dir).map_err(|e| internal_error(e.to_string()))?;
        serde_json::to_writer(&mut tmp, &record).map_err(|e| internal_error(e.to_string()))?;
        tmp.flush()
            .and_then(|()| tmp.as_file().sync_all())
            .map_err(|e| internal_error(e.to_string()))?;
        tmp.persist(&self.path)
            .map_err(|e| internal_error(e.to_string()))?;
        File::open(dir)
            .and_then(|f| f.sync_all())
            .map_err(|e| internal_error(e.to_string()))
    }

    pub(super) fn source_stamp(
        source: &dyn codex_extension_api::ConversationHistorySnapshot,
    ) -> Result<String, JSONRPCErrorError> {
        let mut hash = Sha256::new();
        for item in source.items() {
            let mut value =
                serde_json::to_value(item).map_err(|e| internal_error(e.to_string()))?;
            value.sort_all_objects();
            serde_json::to_writer(&mut hash, &value).map_err(|e| internal_error(e.to_string()))?;
        }
        Ok(format!("{:x}", hash.finalize()))
    }

    pub(super) fn bind(
        &self,
        fork: ThreadId,
        unchanged: Option<String>,
    ) -> Result<(), JSONRPCErrorError> {
        self.save(Capture::Fork {
            fork,
            actor: Some(boot()),
            unchanged,
        })
    }

    pub(crate) async fn start(
        mut task: SampleTask,
        tasks: &tokio_util::task::TaskTracker,
    ) -> Result<ThreadSampleStartResponse, JSONRPCErrorError> {
        if task.params.expected_turn_id.trim().is_empty() || task.params.prompt.trim().is_empty() {
            return Err(invalid_request(
                "a sample requires an expectedTurnId and prompt",
            ));
        }
        let settings = task.parent.thread_settings_snapshot().await;
        if settings.openrouter_transport != OpenRouterTransport::Batch {
            return Err(invalid_request(
                "thread/sample/start requires an OpenRouter batch thread; use thread/sample for live inference",
            ));
        }
        let model = task
            .params
            .model
            .as_ref()
            .unwrap_or(&settings.model)
            .clone();
        let id = Uuid::new_v4();
        let owner = task.parent.startup_metadata().thread_id;
        let handle = Self::new(
            &task.parent.config().await.codex_home,
            owner,
            id,
            task.params.expected_turn_id.clone(),
        )?;
        // Connection cancellation ceases to own the paid job when its handle is returned.
        task.cancellation = tokio_util::sync::CancellationToken::new();
        tasks.spawn(async move {
            // Bound only snapshot acquisition and the lifetime of the ephemeral sampling
            // actor. A collector with a remote job id continues without that actor.
            let deadline = Instant::now() + SAMPLE_TIMEOUT;
            let admission = tokio::time::timeout_at(
                deadline,
                task.parent.validate_openrouter_batch_model(&model),
            )
            .await;
            let result = match admission {
                Ok(Ok(())) => Box::pin(task.run(deadline, Some(&handle))).await,
                Ok(Err(error)) => Err(invalid_request(error.to_string())),
                Err(_) => Err(internal_error("sample timed out")),
            };
            let capture = std::fs::read(&handle.path)
                .ok()
                .and_then(|bytes| serde_json::from_slice::<Record>(&bytes).ok())
                .map(|r| r.capture);
            let outcome = match (capture, result) {
                (Some(Capture::Fork { fork, .. }), Ok(_)) => Capture::Fork {
                    fork,
                    actor: None,
                    unchanged: None,
                },
                (
                    Some(Capture::Fork {
                        fork, unchanged, ..
                    }),
                    Err(error),
                ) if error.message == "sample timed out" => Capture::Fork {
                    fork,
                    actor: None,
                    unchanged,
                },
                (_, Err(error)) => Capture::Failed(error.message),
                (_, Ok(_)) => Capture::Failed("Sample had no captured fork.".into()),
            };
            if let Err(e) = handle.save(outcome) {
                tracing::error!(reason=%e.message,"could not record sample outcome");
            }
        });
        Ok(ThreadSampleStartResponse {
            handle: id.to_string(),
        })
    }

    pub(crate) async fn read(
        parent: &CodexThread,
        params: ThreadSampleReadParams,
    ) -> Result<ThreadSampleReadResponse, JSONRPCErrorError> {
        let id = Uuid::parse_str(&params.handle)
            .map_err(|_| invalid_request("invalid sample handle"))?;
        let path = Self::directory(
            &parent.config().await.codex_home,
            parent.startup_metadata().thread_id,
        )
        .join(id.to_string());
        let deadline =
            Instant::now() + Duration::from_millis(u64::from(params.wait_ms.min(30_000)));
        loop {
            let file =
                File::open(&path).map_err(|_| invalid_request("unknown sample on this thread"))?;
            let record: Record =
                serde_json::from_reader(file).map_err(|e| internal_error(e.to_string()))?;
            match record.capture {
                Capture::Starting(origin) if origin != boot() => {
                    return Ok(ThreadSampleReadResponse::Failed {
                        message: "Server stopped before this sample was admitted.".into(),
                    });
                }
                Capture::Starting(_) => {}
                Capture::Fork {
                    actor: Some(actor), ..
                } if actor == boot() => {}
                Capture::Failed(message) => {
                    return Ok(ThreadSampleReadResponse::Failed { message });
                }
                Capture::Fork {
                    fork, unchanged, ..
                } => {
                    // Read the local receipt. Any necessary provider GET belongs
                    // to the collector and never extends this RPC's wait budget.
                    match parent.read_openrouter_batch_sample(fork).await {
                        Ok(OpenRouterSampleStatus::Ready(events)) => {
                            if let Some(stamp) = unchanged {
                                let current = parent.conversation_history_snapshot().await;
                                if Self::source_stamp(current.as_ref())? != stamp {
                                    return Ok(ThreadSampleReadResponse::Failed {
                                        message: "sample source changed".into(),
                                    });
                                }
                                Self {
                                    path: path.clone(),
                                    source_turn: record.source_turn.clone(),
                                }
                                .save(Capture::Fork {
                                    fork,
                                    actor: None,
                                    unchanged: None,
                                })?;
                            }
                            let mut result = collect_response(
                                futures::stream::iter(events.into_iter().map(Ok)).boxed(),
                            )
                            .await?;
                            result.source_turn_id = record.source_turn;
                            return Ok(ThreadSampleReadResponse::Ready { result });
                        }
                        Ok(OpenRouterSampleStatus::NoSubmission) => {
                            return Ok(ThreadSampleReadResponse::Failed {
                                message: "Sample stopped before submitting a paid request.".into(),
                            });
                        }
                        Ok(OpenRouterSampleStatus::Pending) => {}
                        Err(e) => {
                            return Ok(ThreadSampleReadResponse::Failed {
                                message: e.to_string(),
                            });
                        }
                    }
                }
            }
            if Instant::now() >= deadline {
                return Ok(ThreadSampleReadResponse::Pending);
            }
            tokio::time::sleep_until(deadline.min(Instant::now() + Duration::from_secs(1))).await;
        }
    }
}
