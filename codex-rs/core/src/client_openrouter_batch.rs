//! A paid step has a durable owner before POST. Reads may repeat; submit and
//! admission may not. If a worker dies after admission, even a tool call with
//! no recorded output may already have affected the world.
use super::ModelClient;
use super::openrouter::tools::Projection;
use crate::agent::control::ProviderAccountSelection;
use crate::session::session::Session;
use crate::session::step_context::StepContext;
use codex_api::ResponseStream;
use codex_api::ResponsesApiRequest;
use codex_config::read_openrouter_config;
use codex_model_provider::openrouter::BatchDispatch;
use codex_model_provider::openrouter::BatchId;
use codex_model_provider::openrouter::BatchRoute;
use codex_model_provider::openrouter::BatchUsage;
use codex_model_provider::openrouter::Bearer;
use codex_model_provider::openrouter::Client;
use codex_model_provider::openrouter::Error;
use codex_model_provider::openrouter::LiveReason;
use codex_model_provider::openrouter::Settlement;
use codex_model_provider::openrouter::credentials;
use codex_protocol::KeyName;
use codex_protocol::ThreadId;
use codex_protocol::error::CodexErr;
use codex_protocol::error::Result;
use codex_protocol::openrouter::OpenRouterTransport;
use serde::Deserialize;
use serde::Serialize;
use serde_json::Value;
use serde_json::json;
use sha2::Digest;
use sha2::Sha256;
use std::fs::File;
use std::fs::OpenOptions;
use std::fs::{self};
use std::io::Write;
use std::io::{self};
use std::os::unix::fs::DirBuilderExt;
use std::os::unix::fs::OpenOptionsExt;
use std::path::Path;
use std::path::PathBuf;
use std::sync::Arc;
use std::sync::Mutex;
use std::time::Duration;
use tokio::sync::mpsc;
use uuid::Uuid;
#[path = "openrouter_batch_recovery.rs"]
mod recovery;
pub use recovery::outstanding_openrouter_batches;
pub use recovery::recover_openrouter_batches;

fn fatal(error: impl std::fmt::Display) -> CodexErr {
    CodexErr::Fatal(format!("OpenRouter batch: {error}"))
}

#[derive(Clone, Copy, Serialize, Deserialize)]
pub(crate) enum Operation {
    Generation,
    Compaction,
    StandaloneCompaction,
}

#[derive(Default)]
struct StandaloneCompactionRecovery(Mutex<Option<String>>);

pub(crate) fn arm_standalone(sess: &Session, turn: &str) {
    let slot = sess
        .services
        .thread_extension_data
        .get_or_init(StandaloneCompactionRecovery::default);
    *slot
        .0
        .lock()
        .unwrap_or_else(std::sync::PoisonError::into_inner) = Some(turn.to_owned());
}

pub(crate) fn take_standalone(sess: &Session, turn: &str) -> bool {
    sess.services
        .thread_extension_data
        .get::<StandaloneCompactionRecovery>()
        .is_some_and(|slot| {
            slot.0
                .lock()
                .unwrap_or_else(std::sync::PoisonError::into_inner)
                .take_if(|id| id == turn)
                .is_some()
        })
}

/// Only a sample owner may read its ephemeral fork's paid result.
pub struct OpenRouterSample(pub ThreadId);

pub enum SampleStatus {
    NoSubmission,
    Pending,
    Ready(Vec<codex_api::ResponseEvent>),
}

#[derive(Clone, Serialize, Deserialize)]
pub struct OpenRouterBatchRecovery {
    pub root: ThreadId,
    pub cwd: codex_utils_path_uri::PathUri,
    pub workspace_roots: Vec<codex_utils_path_uri::PathUri>,
    pub prompt_cache_ttl: Option<crate::PromptCacheTtl>,
    pub cyber_access_program: Option<codex_protocol::turn_input::CyberAccessProgram>,
}

pub(crate) struct Step {
    cancellation: tokio_util::sync::CancellationToken,
    recovery: OpenRouterBatchRecovery,
    operation: Operation,
    path: PathBuf,
    settings: Value,
    thread_id: ThreadId,
    turn_id: String,
    sample_for: Option<ThreadId>,
    receipt: Mutex<Option<Lease>>,
}

impl Step {
    pub(crate) async fn capture(
        sess: &Session,
        ctx: &StepContext,
        operation: Operation,
    ) -> Result<Option<Arc<Self>>> {
        if !sess.services.model_client.uses_openrouter()
            && ctx.settings.openrouter_transport() == OpenRouterTransport::Live
        {
            return Ok(None);
        }
        let thread_id = sess.thread_id();
        let turn_id = &ctx.turn.sub_id;
        let dir = turn_directory(&ctx.turn.config.codex_home, thread_id, turn_id);
        let path = dir.join(match operation {
            Operation::Generation => "generation.json",
            Operation::Compaction | Operation::StandaloneCompaction => "compaction.json",
        });
        let other = match operation {
            Operation::Generation => "compaction.json",
            Operation::Compaction | Operation::StandaloneCompaction => "generation.json",
        };
        if load(&dir.join(other))?
            .is_some_and(|job| !matches!(job.phase, Phase::Consumed | Phase::Rejected(_)))
        {
            return Err(fatal(
                "a different paid operation owns this turn; not submitting",
            ));
        }
        if ctx.settings.openrouter_transport() == OpenRouterTransport::Live {
            // Turning slow mode off cannot abandon or silently upgrade an admitted step.
            match load(&path)? {
                Some(Journal {
                    phase:
                        Phase::Submitting
                        | Phase::Submitted(_)
                        | Phase::Settled { .. }
                        | Phase::Admitting { .. },
                    ..
                }) => {}
                _ => return Ok(None),
            }
        }
        if !sess.services.model_client.uses_openrouter() {
            return Err(fatal("requires an OpenRouter model"));
        }
        let environment = ctx
            .environments
            .single_local_environment()
            .map(crate::session::turn_context::TurnEnvironment::selection)
            .filter(|e| e.config == codex_protocol::protocol::EnvironmentConfigState::FromThread)
            .ok_or_else(|| {
                fatal("batch recovery requires a persistent local thread environment")
            })?;
        let sample = sess
            .services
            .thread_extension_data
            .get::<OpenRouterSample>();
        if ctx.turn.config.ephemeral && sample.is_none() {
            return Err(fatal("an ephemeral thread cannot own an OpenRouter batch"));
        }
        let history = sess.conversation_history_snapshot().await;
        let mut revision = Sha256::new();
        for item in history.items() {
            let mut value = serde_json::to_value(item).map_err(fatal)?;
            value.sort_all_objects();
            serde_json::to_writer(&mut revision, &value).map_err(fatal)?;
        }
        let revision = format!("{:x}", revision.finalize());
        sess.flush_rollout().await.map_err(fatal)?;
        sess.send_event(
            &ctx.turn,
            codex_protocol::protocol::EventMsg::WorkStatusUpdated(
                codex_protocol::protocol::WorkStatus {
                    turn_id: turn_id.clone(),
                    activity: "Waiting for OpenRouter batch".into(),
                    emitted_at: Some(chrono::Utc::now().timestamp()),
                },
            ),
        )
        .await;
        Ok(Some(Arc::new(Self {
            cancellation: ctx.turn_cancellation.clone(),
            recovery: OpenRouterBatchRecovery {
                root: sess.session_id().into(),
                cwd: environment.cwd,
                workspace_roots: environment.workspace_roots,
                prompt_cache_ttl: ctx.turn.prompt_cache_ttl,
                cyber_access_program: ctx.turn.cyber_access_program,
            },
            operation,
            path,
            thread_id,
            turn_id: turn_id.clone(),
            sample_for: sample.map(|sample| sample.0),
            receipt: Mutex::default(),
            settings: json!({
                "history_revision":revision,
                "model":ctx.settings.model_info.slug, "effort":ctx.settings.effective_reasoning_effort(), "approval":ctx.settings.approval_policy(),
                "reviewer":ctx.settings.approvals_reviewer(), "permissions":ctx.turn.permission_profile_for_environments(&ctx.environments),
            }),
        })))
    }

    /// Called only after the ordinary executor has awaited every dispatched tool
    /// and flushed its output. No journal transition starts or executes a tool.
    pub(crate) async fn consumed(&self, sess: &Session) -> Result<()> {
        sess.flush_rollout().await.map_err(fatal)?;
        let mut slot = self
            .receipt
            .lock()
            .map_err(|_| fatal("receipt lock poisoned"))?;
        let mut lease = slot
            .take()
            .ok_or_else(|| fatal("missing admission receipt"))?;
        let Phase::Admitting {
            ref job,
            ref settlement,
        } = lease.journal.phase
        else {
            return Err(fatal("step was not admitted"));
        };
        lease.journal.history.push(Charge {
            payer: lease.journal.key.clone(),
            fingerprint: lease.journal.fingerprint.clone(),
            request: lease.journal.request.clone(),
            job: job.clone(),
            usage: settlement.usage.clone(),
        });
        lease.journal.phase = Phase::Consumed;
        lease.save()
    }

    pub(crate) async fn clear_waiting(&self, sess: &Session, ctx: &StepContext) {
        sess.send_event(
            &ctx.turn,
            codex_protocol::protocol::EventMsg::WorkStatusUpdated(
                codex_protocol::protocol::WorkStatus {
                    turn_id: self.turn_id.clone(),
                    activity: String::new(),
                    emitted_at: Some(chrono::Utc::now().timestamp()),
                },
            ),
        )
        .await;
    }
}

fn turn_directory(home: &Path, thread: ThreadId, turn: &str) -> PathBuf {
    home.join("openrouter-batches")
        .join(thread.to_string())
        .join(format!("{:x}", Sha256::digest(turn.as_bytes())))
}

fn admission_lock(dir: &Path) -> Result<File> {
    let file = OpenOptions::new()
        .read(true)
        .write(true)
        .create(true)
        .truncate(false)
        .mode(0o600)
        .open(dir.join("admission.lock"))
        .map_err(fatal)?;
    file.lock().map_err(fatal)?;
    Ok(file)
}

pub(crate) fn has_paid_step(home: &Path, thread: ThreadId, turn: &str) -> Result<bool> {
    let dir = turn_directory(home, thread, turn);
    for entry in ["generation.json", "compaction.json"] {
        if load(&dir.join(entry))?
            .is_some_and(|j| !matches!(j.phase, Phase::Consumed | Phase::Rejected(_)))
        {
            return Ok(true);
        }
    }
    Ok(false)
}

pub(crate) fn has_standalone_compaction(home: &Path, thread: ThreadId, turn: &str) -> Result<bool> {
    Ok(
        load(&turn_directory(home, thread, turn).join("compaction.json"))?.is_some_and(|journal| {
            matches!(journal.operation, Operation::StandaloneCompaction)
                && !matches!(journal.phase, Phase::Consumed | Phase::Rejected(_))
        }),
    )
}

pub(crate) async fn park(sess: &Session, turn_id: &str) -> Result<bool> {
    let (turn, handle) = {
        let mut active = sess.active_turn.lock().await;
        let Some(task) = active
            .as_ref()
            .and_then(|turn| turn.task.as_ref())
            .filter(|task| task.turn_context.sub_id == turn_id)
        else {
            return Ok(false);
        };
        let dir = turn_directory(
            &task.turn_context.config.codex_home,
            sess.thread_id(),
            turn_id,
        );
        if !dir.try_exists().map_err(fatal)? {
            return Ok(false);
        }
        let guard = admission_lock(&dir)?;
        let mut parkable = false;
        for name in ["generation.json", "compaction.json"] {
            if let Some(job) = load(&dir.join(name))? {
                match job.phase {
                    Phase::Submitting | Phase::Submitted(_) | Phase::Settled { .. } => {
                        parkable = true;
                    }
                    Phase::Admitting { .. } => return Ok(false),
                    Phase::Consumed | Phase::Rejected(_) => {}
                }
            }
        }
        if !parkable || dir.join("abandoned").try_exists().map_err(fatal)? {
            return Ok(false);
        }
        OpenOptions::new()
            .write(true)
            .create(true)
            .truncate(false)
            .mode(0o600)
            .open(dir.join("parked"))
            .and_then(|f| f.sync_all())
            .map_err(fatal)?;
        let mut turn = active.take().ok_or_else(|| fatal("parked turn missing"))?;
        let task = turn
            .task
            .take()
            .ok_or_else(|| fatal("parked task missing"))?;
        drop(task.outcome_handoff);
        task.cancellation_token.cancel();
        task.turn_context
            .turn_metadata_state
            .cancel_git_enrichment_task();
        task.handle.abort();
        drop(guard);
        (turn, task.handle)
    };
    let _ = handle.await;
    sess.input_queue.clear_pending(&turn).await;
    sess.flush_rollout().await.map_err(fatal)?;
    Ok(true)
}

pub(crate) fn unpark(home: &Path, thread: ThreadId, turn: &str) -> Result<()> {
    let dir = turn_directory(home, thread, turn);
    if dir.join("abandoned").try_exists().map_err(fatal)? {
        return Err(CodexErr::TurnAborted);
    }
    match fs::remove_file(dir.join("parked")) {
        Ok(()) => Ok(()),
        Err(error) if error.kind() == io::ErrorKind::NotFound => Ok(()),
        Err(error) => Err(fatal(error)),
    }
}

/// An interrupt revokes only local delivery. The poller still retrieves accounting and deletes artifacts.
pub(crate) fn abandon(home: &Path, thread: ThreadId, turn: &str) -> Result<()> {
    let dir = turn_directory(home, thread, turn);
    if !dir.try_exists().map_err(fatal)? {
        return Ok(());
    }
    let file = OpenOptions::new()
        .write(true)
        .create(true)
        .truncate(false)
        .mode(0o600)
        .open(dir.join("abandoned"))
        .map_err(fatal)?;
    file.sync_all()
        .and_then(|()| File::open(&dir)?.sync_all())
        .map_err(fatal)
}

#[derive(Serialize, Deserialize)]
struct Charge {
    payer: KeyName,
    fingerprint: String,
    request: String,
    job: Execution,
    usage: Option<BatchUsage>,
}

#[derive(Clone, Serialize, Deserialize)]
struct Artifact {
    job: BatchId,
    payer: KeyName,
    fingerprint: String,
    base_url: String,
}

#[derive(Clone, Serialize, Deserialize)]
enum Execution {
    Batch(BatchId),
    Live(LiveReason),
}

#[derive(Serialize, Deserialize)]
enum Phase {
    Submitting,
    Submitted(BatchId),
    Settled {
        job: Execution,
        settlement: Settlement,
    },
    Admitting {
        job: Execution,
        settlement: Settlement,
    },
    Consumed,
    Rejected(String),
}

#[derive(Serialize, Deserialize)]
struct Journal {
    operation: Operation,
    recovery: OpenRouterBatchRecovery,
    dispatch: BatchDispatch,
    sample_for: Option<ThreadId>,
    thread: ThreadId,
    turn: String,
    key: KeyName,
    fingerprint: String,
    base_url: String,
    route: BatchRoute,
    authority: codex_protocol::EndpointFilter,
    request: String,
    settings: Value,
    projection: Value,
    custom_id: String,
    phase: Phase,
    history: Vec<Charge>,
    #[serde(default)]
    cleanup: Vec<Artifact>,
}

fn load(path: &Path) -> Result<Option<Journal>> {
    match File::open(path) {
        Ok(file) => serde_json::from_reader(file).map(Some).map_err(fatal),
        Err(e) if e.kind() == io::ErrorKind::NotFound => Ok(None),
        Err(e) => Err(fatal(e)),
    }
}

struct Lease {
    _lock: File,
    path: PathBuf,
    journal: Journal,
}

impl Lease {
    fn lock(path: &Path) -> Result<File> {
        let dir = path.parent().ok_or_else(|| fatal("invalid journal path"))?;
        fs::DirBuilder::new()
            .recursive(true)
            .mode(0o700)
            .create(dir)
            .map_err(fatal)?;
        let lock = OpenOptions::new()
            .read(true)
            .write(true)
            .create(true)
            .truncate(false)
            .mode(0o600)
            .open(path.with_extension("lock"))
            .map_err(fatal)?;
        lock.try_lock()
            .map_err(|_| fatal("step has another running owner; no new submission was made"))?;
        Ok(lock)
    }

    fn save(&self) -> Result<()> {
        let dir = self
            .path
            .parent()
            .ok_or_else(|| fatal("invalid journal path"))?;
        let mut temporary = tempfile::NamedTempFile::new_in(dir).map_err(fatal)?;
        serde_json::to_writer(&mut temporary, &self.journal).map_err(fatal)?;
        temporary
            .flush()
            .and_then(|()| temporary.as_file().sync_all())
            .map_err(fatal)?;
        temporary.persist(&self.path).map_err(fatal)?;
        File::open(dir)
            .and_then(|dir| dir.sync_all())
            .map_err(fatal)
    }

    fn abandoned(&self) -> Result<bool> {
        self.path
            .with_file_name("abandoned")
            .try_exists()
            .map_err(fatal)
    }

    /// Make the result and the cleanup obligation durable in the same write.
    /// A failed DELETE never invalidates the answer or licenses another POST.
    async fn settle(
        &mut self,
        client: &Client,
        bearer: &Bearer,
        job: &BatchId,
        settlement: Settlement,
    ) -> Result<()> {
        self.journal.cleanup.push(Artifact {
            job: job.clone(),
            payer: self.journal.key.clone(),
            fingerprint: self.journal.fingerprint.clone(),
            base_url: self.journal.base_url.clone(),
        });
        self.journal.phase = Phase::Settled {
            job: Execution::Batch(job.clone()),
            settlement,
        };
        self.save()?;
        self.cleanup(client, bearer, job).await
    }

    async fn cleanup(&mut self, client: &Client, bearer: &Bearer, job: &BatchId) -> Result<()> {
        match client.delete_batch(bearer, job).await {
            Ok(()) | Err(Error::Http(http::StatusCode::NOT_FOUND)) => {
                self.journal.cleanup.retain(|artifact| &artifact.job != job);
                self.save()
            }
            Err(error) => {
                tracing::warn!(%error,"OpenRouter result saved; remote deletion remains pending");
                Ok(())
            }
        }
    }

    async fn poll(&mut self, client: &Client, bearer: &Bearer, job: &BatchId) -> Result<()> {
        let mut delay = Duration::from_secs(2);
        loop {
            if self
                .path
                .with_file_name("parked")
                .try_exists()
                .map_err(fatal)?
            {
                return Err(CodexErr::TurnAborted);
            }
            match client
                .read_batch(bearer, job, &self.journal.custom_id)
                .await
            {
                Ok(Some(settlement)) => {
                    return self.settle(client, bearer, job, settlement).await;
                }
                Ok(None) | Err(Error::Timeout | Error::Transport) => {}
                Err(Error::Http(status))
                    if status.is_server_error()
                        || matches!(
                            status,
                            http::StatusCode::NOT_FOUND
                                | http::StatusCode::TOO_MANY_REQUESTS
                                | http::StatusCode::REQUEST_TIMEOUT
                        ) => {}
                Err(e) => return Err(fatal(e)),
            }
            tokio::time::sleep(delay).await;
            delay = (delay * 2).min(Duration::from_secs(60));
        }
    }
}

impl ModelClient {
    fn batch_client(&self) -> Result<(Client, String, KeyName)> {
        if !self.uses_openrouter() {
            return Err(fatal("requires an OpenRouter model"));
        }
        let Some(ProviderAccountSelection::OpenRouter { key }) = self.provider_account_selection()
        else {
            return Err(fatal("select an OpenRouter key with /account"));
        };
        let base_url = self
            .state
            .provider
            .info()
            .base_url
            .clone()
            .unwrap_or_else(|| "https://openrouter.ai/api/v1".into());
        let client = Client::new(&base_url, self.http_client_factory.clone()).map_err(fatal)?;
        Ok((client, base_url, key))
    }

    pub(crate) async fn validate_batch_selection(&self, model: &str) -> Result<()> {
        let (client, _, key) = self.batch_client()?;
        let home = self.openrouter_home()?;
        let preferences = read_openrouter_config(&home).await.map_err(fatal)?;
        let slug = model.parse().map_err(fatal)?;
        let filter = preferences
            .models
            .get(&slug)
            .ok_or_else(|| fatal("save the ordinary model before choosing batch"))?;
        let bearer = credentials(&home, &key).await.map_err(fatal)?;
        client
            .batch_route(&slug, filter, &bearer)
            .await
            .map_err(fatal)?;
        Ok(())
    }

    pub(super) async fn open_batch_step(
        &self,
        step: &Step,
        request: ResponsesApiRequest,
        projection: Option<&Projection>,
    ) -> Result<(Vec<u8>, ResponseStream)> {
        let (client, base_url, selected_key) = self.batch_client()?;
        let home = self.openrouter_home()?;
        let projection = serde_json::to_value(
            projection.ok_or_else(|| fatal("missing OpenRouter tool projection"))?,
        )
        .map_err(fatal)?;
        let mut identity = serde_json::to_value(&request).map_err(fatal)?;
        // Outbound trace IDs are not part of the model prompt and change across restart.
        let object = identity
            .as_object_mut()
            .ok_or_else(|| fatal("invalid Responses request"))?;
        object.remove("client_metadata");
        identity.sort_all_objects();
        let digest = format!(
            "{:x}",
            Sha256::digest(serde_json::to_vec(&identity).map_err(fatal)?)
        );
        let lock = Lease::lock(&step.path)?;
        let existing = load(&step.path)?;
        let key = selected_key;
        let bearer = credentials(&home, &key).await.map_err(fatal)?;
        let fingerprint = bearer.batch_fingerprint();
        let (journal, wire, new) = match existing {
            Some(journal) if !matches!(journal.phase, Phase::Consumed) => {
                if journal.base_url != base_url
                    || journal.thread != step.thread_id
                    || journal.turn != step.turn_id
                    || journal.key != key
                    || journal.fingerprint != fingerprint
                    || journal.request != digest
                    || journal.settings != step.settings
                    || journal.projection != projection
                {
                    // Changed history or authority vetoes delivery, not an invoice.
                    // Only the recorded origin and credential may retrieve an old job.
                    if journal.base_url == base_url
                        && journal.thread == step.thread_id
                        && journal.turn == step.turn_id
                        && matches!(journal.phase, Phase::Submitted(_))
                    {
                        abandon(&home, step.thread_id, &step.turn_id)?;
                        drop(lock);
                        recovery::collect_detached(home, step.path.clone(), client, journal);
                    }
                    return Err(fatal(
                        "pending step no longer matches its captured history, tools, settings or payer; not submitting or delivering",
                    ));
                }
                match &journal.phase {
                    Phase::Submitting => {
                        return Err(fatal("submission outcome is unknown; do not resubmit"));
                    }
                    Phase::Admitting { .. } => {
                        return Err(fatal(
                            "prior tool delivery may have affected the world; inspect its outcome before continuing",
                        ));
                    }
                    Phase::Rejected(reason) => return Err(fatal(reason)),
                    _ => {}
                }
                let wire = journal
                    .route
                    .encode(&journal.custom_id, request, journal.dispatch)
                    .map_err(fatal)?;
                (journal, wire, false)
            }
            prior => {
                let (history, cleanup) = prior
                    .map(|journal| (journal.history, journal.cleanup))
                    .unwrap_or_default();
                if history.iter().any(|charge| charge.request == digest) {
                    return Err(fatal(
                        "this step was already consumed; refusing to repeat it",
                    ));
                }
                let slug = request.model.parse().map_err(fatal)?;
                let prefs = read_openrouter_config(&home).await.map_err(fatal)?;
                let saved = prefs
                    .models
                    .get(&slug)
                    .ok_or_else(|| fatal("save the ordinary model before choosing batch"))?;
                if Some(saved) != request.openrouter.as_ref().map(|routing| &routing.0) {
                    return Err(fatal("model route changed before batch admission"));
                }
                let route = client
                    .batch_route(&slug, saved, &bearer)
                    .await
                    .map_err(fatal)?;
                let custom_id = Uuid::new_v4().to_string();
                let dispatch = route
                    .dispatch(&request, !matches!(step.operation, Operation::Generation))
                    .map_err(fatal)?;
                let wire = route.encode(&custom_id, request, dispatch).map_err(fatal)?;
                let journal = Journal {
                    operation: step.operation,
                    recovery: step.recovery.clone(),
                    dispatch,
                    sample_for: step.sample_for,
                    thread: step.thread_id,
                    turn: step.turn_id.clone(),
                    key,
                    fingerprint,
                    base_url,
                    route,
                    authority: saved.clone(),
                    request: digest,
                    settings: step.settings.clone(),
                    projection,
                    custom_id,
                    phase: Phase::Submitting,
                    history,
                    cleanup,
                };
                (journal, wire, true)
            }
        };
        let mut lease = Lease {
            _lock: lock,
            path: step.path.clone(),
            journal,
        };
        if step.cancellation.is_cancelled() || lease.abandoned()? {
            return Err(CodexErr::TurnAborted);
        }
        if new {
            lease.save()?;
        }
        let trace_wire = wire.clone();
        let (tx, rx) = tokio::sync::oneshot::channel();
        // The collector outlives transport cancellation. An abandoned turn can still incur a charge.
        tokio::spawn(async move {
            let result = async {
                if new && let BatchDispatch::NecessaryLive(reason) = lease.journal.dispatch {
                    let settlement = client
                        .necessary_live_response(&bearer, wire)
                        .await
                        .map_err(|_| {
                            fatal("necessary live compaction outcome is unknown; no retry was made")
                        })?;
                    lease.journal.phase = Phase::Settled {
                        job: Execution::Live(reason),
                        settlement,
                    };
                    lease.save()?;
                } else if new {
                    match client.submit_batch(&bearer, wire).await {
                        Ok(job) => {
                            lease.journal.phase = Phase::Submitted(job);
                            lease.save()?;
                        }
                        Err(Error::Http(status))
                            if status.is_client_error()
                                && status != http::StatusCode::REQUEST_TIMEOUT =>
                        {
                            let reason =
                                format!("submission rejected (HTTP {status}); no automatic retry");
                            lease.journal.phase = Phase::Rejected(reason.clone());
                            lease.save()?;
                            return Err(fatal(reason));
                        }
                        Err(_) => {
                            return Err(fatal("submission outcome is unknown; do not resubmit"));
                        }
                    }
                }
                if let Phase::Submitted(job) = &lease.journal.phase {
                    let job = job.clone();
                    lease.poll(&client, &bearer, &job).await?;
                }
                Ok(lease)
            }
            .await;
            let _ = tx.send(result);
        });
        let mut lease = rx.await.map_err(fatal)??;
        // An administrator may have forgotten the model or narrowed the route
        // while this row was remote. Reconcile its invoice, but never execute
        // its returned tools under an invalidated admission.
        let current = read_openrouter_config(&home).await.map_err(fatal)?;
        let payer = credentials(&home, &lease.journal.key)
            .await
            .map_err(fatal)?;
        if payer.batch_fingerprint() != lease.journal.fingerprint
            || current.models.get(lease.journal.route.model()) != Some(&lease.journal.authority)
        {
            return Err(fatal(
                "captured endpoint policy or payer changed while the step was remote; not delivering",
            ));
        }
        let _guard = admission_lock(
            lease
                .path
                .parent()
                .ok_or_else(|| fatal("invalid journal path"))?,
        )?;
        if lease.abandoned()?
            || lease
                .path
                .with_file_name("parked")
                .try_exists()
                .map_err(fatal)?
        {
            return Err(CodexErr::TurnAborted);
        }
        let Phase::Settled { job, settlement } = &lease.journal.phase else {
            return Err(fatal("missing settlement"));
        };
        let events = self.settled_events(settlement, lease.journal.route.model().as_str())?;
        let (tx, rx_event) = mpsc::channel(events.len().max(1));
        lease.journal.phase = Phase::Admitting {
            job: job.clone(),
            settlement: settlement.clone(),
        };
        lease.save()?;
        *step
            .receipt
            .lock()
            .map_err(|_| fatal("admission receipt poisoned"))? = Some(lease);
        for event in events {
            tx.try_send(Ok(event)).map_err(fatal)?;
        }
        Ok((
            trace_wire,
            ResponseStream {
                routing_affinity: None,
                rx_event,
                upstream_request_id: None,
                interrupt: None,
            },
        ))
    }

    fn settled_events(
        &self,
        settlement: &Settlement,
        model: &str,
    ) -> Result<Vec<codex_api::ResponseEvent>> {
        let mut body = settlement.response.clone().map_err(fatal)?;
        if let Some(usage) = &settlement.usage {
            let units = body.get_mut("usage").filter(|v| v.is_object());
            if let Some(units) = units {
                if let Some(cost) = usage.cost {
                    units["cost"] = json!(cost);
                }
            } else {
                body["usage"] = json!({"input_tokens":usage.prompt_tokens,"output_tokens":usage.completion_tokens,"total_tokens":usage.total_tokens,"cost":usage.cost});
            }
        }
        codex_api::openrouter::batch_response_events(body)
            .map_err(|e| self.map_request_error(e, model))
    }

    pub(crate) async fn read_batch_sample(&self, fork: ThreadId) -> Result<SampleStatus> {
        let home = self.openrouter_home()?;
        let root = home.join("openrouter-batches").join(fork.to_string());
        let mut paths = Vec::new();
        match fs::read_dir(&root) {
            Ok(dirs) => {
                for entry in dirs {
                    let file = entry.map_err(fatal)?.path().join("generation.json");
                    if file.try_exists().map_err(fatal)? {
                        paths.push(file);
                    }
                }
            }
            Err(e) if e.kind() == io::ErrorKind::NotFound => return Ok(SampleStatus::NoSubmission),
            Err(e) => return Err(fatal(e)),
        }
        if paths.is_empty() {
            return Ok(SampleStatus::NoSubmission);
        }
        let [path]: [PathBuf; 1] = paths
            .try_into()
            .map_err(|_| fatal("ambiguous sample journal"))?;
        let Some(journal) = load(&path)? else {
            return Ok(SampleStatus::NoSubmission);
        };
        let (client, base_url, key) = self.batch_client()?;
        if journal.sample_for != Some(self.state.thread_id)
            || journal.thread != fork
            || journal.key != key
            || journal.base_url != base_url
        {
            return Err(fatal("sample does not belong to this thread and payer"));
        }
        let bearer = credentials(&home, &key).await.map_err(fatal)?;
        if bearer.batch_fingerprint() != journal.fingerprint {
            return Err(fatal("sample payer changed"));
        }
        match &journal.phase {
            Phase::Settled { settlement, .. } | Phase::Admitting { settlement, .. } => {
                return self
                    .settled_events(settlement, journal.route.model().as_str())
                    .map(SampleStatus::Ready);
            }
            Phase::Rejected(e) => return Err(fatal(e)),
            Phase::Consumed => return Err(fatal("sample no longer available")),
            Phase::Submitting | Phase::Submitted(_) => {}
        }
        // An active collector holds this lock through durable settlement. After a restart
        // one reader can safely resume GET; it cannot submit a model request.
        let Ok(lock) = Lease::lock(&path) else {
            return Ok(SampleStatus::Pending);
        };
        let journal = load(&path)?.ok_or_else(|| fatal("sample journal disappeared"))?;
        match &journal.phase {
            Phase::Submitting => Err(fatal(
                "sample submission outcome is unknown; no retry was made",
            )),
            Phase::Submitted(job) => {
                let job = job.clone();
                let mut lease = Lease {
                    _lock: lock,
                    path,
                    journal,
                };
                tokio::spawn(async move {
                    if let Err(error) = lease.poll(&client, &bearer, &job).await {
                        tracing::warn!(%error,"could not reconcile detached sample");
                    }
                });
                Ok(SampleStatus::Pending)
            }
            Phase::Settled { settlement, .. } | Phase::Admitting { settlement, .. } => self
                .settled_events(settlement, journal.route.model().as_str())
                .map(SampleStatus::Ready),
            Phase::Rejected(e) => Err(fatal(e)),
            Phase::Consumed => Err(fatal("sample no longer available")),
        }
    }
}
