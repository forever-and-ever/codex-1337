//! Prepare the live session or replay fallback before transferring the local thread view.

use super::*;
use crate::app_server_session::ResumeModelSettings;
use crate::backend_id::BackendId;
use crate::chatwidget::ReadOnlyThread;
use codex_app_server_protocol::Thread;
use codex_app_server_protocol::ThreadStatus;
use futures::future::BoxFuture;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) struct Target {
    pub(crate) backend_id: BackendId,
    pub(super) parent: ThreadId,
    pub(crate) child: ThreadId,
}

impl Target {
    pub(super) fn key(self) -> (BackendId, ThreadId) {
        (self.backend_id, self.child)
    }
}

#[derive(Debug)]
pub(crate) enum PreparedAttachment {
    Live(Box<AppServerStartedThread>),
    ReplayOnly {
        thread: Box<Thread>,
        cwd: Option<codex_utils_path_uri::PathUri>,
        read_only: Option<ReadOnlyThread>,
    },
}

#[derive(Debug)]
pub(crate) struct Prepared {
    pub(super) result: Result<PreparedAttachment, String>,
    pub(super) metadata: crate::app_server_session::AttachmentMetadata,
    pub(super) close_check: Result<bool, String>,
}

/// A close identifies a thread, but multiple runtimes can have used that thread ID.
#[derive(Default, PartialEq, Eq)]
pub(super) enum CloseState {
    #[default]
    Open,
    Reported,
    Checking,
    Closed,
}

/// Live notifications received after an attachment request began. They take precedence
/// over the earlier thread snapshot when the subscription is installed.
#[derive(Default)]
pub(super) struct SubscriptionUpdates {
    pub(super) settings: Option<codex_app_server_protocol::ThreadSettings>,
    pub(super) turn_changed: bool,
    pub(super) read_state_changed: bool,
    pub(super) close: CloseState,
}

/// Owns the input buffer and unsubscribe request for one attachment attempt.
pub(super) struct Incoming {
    pub(super) received: SubscriptionUpdates,
    created_channel: bool,
    store: Arc<Mutex<ThreadEventStore>>,
    release: BoxFuture<'static, Result<()>>,
}

impl Incoming {
    pub(super) async fn begin(
        app: &mut App,
        server: &mut AppServerSession,
        target: Target,
    ) -> Self {
        let created_channel = !app.thread_event_channels.contains_key(&target.child);
        let channel = app.ensure_thread_channel(target.child);
        channel.backend_id = Some(target.backend_id);
        let store = channel.store.clone();
        {
            let mut events = store.lock().await;
            events.attachment_prefix = Some(events.buffer.len());
        }
        Self {
            received: SubscriptionUpdates::default(),
            created_channel,
            store,
            release: {
                let handle = server.request_handle();
                Box::pin(async move {
                    crate::app::turn_submissions::detach::unsubscribe(
                        &handle,
                        Uuid::new_v4(),
                        target.child,
                    )
                    .await
                    .map_err(Into::into)
                })
            },
        }
    }

    pub(super) fn uses_channel(&self, channel: &ThreadEventChannel) -> bool {
        Arc::ptr_eq(&channel.store, &self.store)
    }

    pub(super) fn owns_channel(
        &self,
        channel: Option<&ThreadEventChannel>,
        owner: BackendId,
    ) -> bool {
        channel
            .is_some_and(|channel| channel.backend_id == Some(owner) && self.uses_channel(channel))
    }

    /// Drop our provisional channel and return the release for an unclaimed live subscription.
    pub(super) async fn discard(
        self,
        app: &mut App,
        target: Target,
        subscribed: bool,
    ) -> Option<BoxFuture<'static, Result<()>>> {
        let child = target.child;
        let same_channel =
            self.owns_channel(app.thread_event_channels.get(&child), target.backend_id);
        self.store.lock().await.attachment_prefix = None;
        let claimed = app
            .thread_event_channels
            .get(&child)
            .is_some_and(|channel| {
                channel
                    .backend_id
                    .is_none_or(|owner| owner == target.backend_id)
                    && (!self.uses_channel(channel)
                        || app.active_thread_id == Some(child)
                        || app.side_threads.contains_key(&child))
            });
        let release =
            subscribed && !claimed && !app.agents_overview.dispatched_requests.contains_key(&child);
        if same_channel && self.created_channel && !claimed {
            // A later resume can replay the same still-pending approval after we give up this buffer.
            for request in self.store.lock().await.pending_replay_requests() {
                app.pending_app_server_requests
                    .resolve_notification(&child.to_string(), request.id());
            }
            app.thread_event_channels.remove(&child);
        }
        release.then_some(self.release)
    }
}

pub(super) async fn prepare_saved(
    mut view: AppServerSession,
    settings: &crate::local_settings::LocalSettings,
    config: &Config,
    child: ThreadId,
) -> Prepared {
    let result = async {
        let mode = match view.thread_read(child, /*include_turns*/ false).await {
            Ok(thread) if thread.status == ThreadStatus::NotLoaded => {
                ResumeModelSettings::PreserveSideConversation
            }
            Err(error) if App::is_terminal_thread_read_error(&error) => {
                return Err(error);
            }
            _ => ResumeModelSettings::PreserveExistingThread,
        };
        prepare(&mut view, settings, config, child, mode).await
    }
    .await
    .map_err(
        |err| match err.downcast_ref::<codex_app_server_client::TypedRequestError>() {
            Some(codex_app_server_client::TypedRequestError::Server { source, .. }) => {
                source.message.clone()
            }
            _ => err.to_string(),
        },
    );
    let metadata = view.take_attachment_metadata(child);
    Prepared {
        result,
        metadata,
        close_check: Ok(false),
    }
}

pub(super) async fn prepare(
    app_server: &mut AppServerSession,
    local_settings: &crate::local_settings::LocalSettings,
    config: &Config,
    thread_id: ThreadId,
    resume_settings: ResumeModelSettings,
) -> Result<PreparedAttachment> {
    let mut resume_config = config.clone();
    if resume_settings == ResumeModelSettings::PreserveSideConversation {
        resume_config.developer_instructions = Some(
            codex_context_fragments::SideConversationPolicy::Attached
                .instructions(resume_config.developer_instructions.as_deref()),
        );
    }
    match app_server
        .resume_thread(local_settings, resume_config, thread_id, resume_settings)
        .await
    {
        Ok(started) => Ok(PreparedAttachment::Live(Box::new(started))),
        // Once subscribed, a second unanchored read could overlap live events.
        Err(err) if app_server.attachment_subscribed == Some(true) => Err(err),
        Err(resume_err) => {
            tracing::warn!(
                thread_id = %thread_id,
                error = %resume_err,
                "failed to resume live thread for selection; falling back to thread/read"
            );
            let mut thread = app_server
                .thread_read(thread_id, /*include_turns*/ false)
                .await?;
            if thread.ephemeral {
                return Err(color_eyre::eyre::eyre!(
                    "Agent thread {thread_id} is not yet available for replay or live attach."
                ));
            }
            match app_server
                .hydrate_initial_thread_history(
                    &mut thread,
                    /*turn_cursor*/ None,
                    /*item_cursor*/ None,
                    Some(config),
                    Some(local_settings),
                    crate::app_server_session::HistoryHydrationScope::Initial,
                )
                .await
            {
                Ok(()) => {}
                Err(err) if App::can_fallback_from_include_turns_error(&err) => {}
                Err(err) => return Err(err),
            }
            if thread.turns.is_empty() {
                // A `thread/read` fallback without turns would create a blank local replay
                // channel with no live listener attached, which blocks later real re-attach.
                return Err(color_eyre::eyre::eyre!(
                    "Agent thread {thread_id} is not yet available for replay or live attach."
                ));
            }
            let convention = app_server.path_convention();
            let cwd = match crate::app_server_session::decode_thread_cwd(&thread.cwd, convention) {
                Ok(cwd) => Some(cwd),
                Err(error) if convention.is_some() => return Err(error.into()),
                Err(_) => None,
            };
            Ok(PreparedAttachment::ReplayOnly {
                thread: Box::new(thread),
                cwd,
                read_only: if crate::session_start::is_archived_session_error(
                    &resume_err,
                    thread_id,
                ) {
                    Some(ReadOnlyThread::Archived)
                } else if crate::app_server_session::is_active_writer_error(&resume_err) {
                    Some(ReadOnlyThread::ExternalWriter)
                } else {
                    None
                },
            })
        }
    }
}

impl App {
    pub(super) async fn install_saved_chat(
        &mut self,
        tui: &mut tui::Tui,
        server: &mut AppServerSession,
        target: Target,
        attachment: PreparedAttachment,
        metadata: crate::app_server_session::AttachmentMetadata,
        updates: &SubscriptionUpdates,
    ) -> Result<()> {
        color_eyre::eyre::ensure!(
            server.backend_id() == target.backend_id,
            "saved chat belongs to a different backend"
        );
        let Target { parent, child, .. } = target;
        server.install_attachment_metadata(child, metadata, updates.read_state_changed);
        self.side_threads.insert(
            child,
            side::SideThreadState {
                storage: crate::app_server_session::SideChatStorage::Attached,
                ..side::SideThreadState::new(parent)
            },
        );
        let closed = updates.close == CloseState::Closed;
        let (mut session, turns, live, read_only) = match attachment {
            PreparedAttachment::Live(started) => {
                if started.blocks_direct_input {
                    self.agent_navigation.mark_parent_owned(child);
                }
                (started.session, started.turns, !closed, None)
            }
            PreparedAttachment::ReplayOnly {
                thread,
                cwd,
                read_only,
            } => {
                let mut session = self
                    .session_state_for_thread_read(child, &thread, cwd)
                    .await;
                if let Some(channel) = self.thread_event_channels.get(&child)
                    && let Some(cached) = channel.store.lock().await.session.as_ref()
                {
                    session.approval_policy = cached.approval_policy;
                    session.permission_profile = cached.permission_profile.clone();
                    session.active_permission_profile = cached.active_permission_profile.clone();
                    session.approvals_reviewer = cached.approvals_reviewer;
                }
                session.model.clear();
                (session, thread.turns, false, read_only)
            }
        };
        session.forked_from_id = None;
        session.fork_parent_title = None;
        session.daybreak_enabled = false;
        if let Some(settings) = &updates.settings
            && let Ok(cwd) = crate::app_server_session::decode_thread_cwd(
                &settings.cwd,
                server.path_convention(),
            )
        {
            super::thread_settings::apply_thread_settings_to_session(&mut session, settings, &cwd);
        }
        let channel = self.ensure_thread_channel(child);
        let (turn_state, replay_output) = {
            let mut store = channel.store.lock().await;
            let turn_state = updates.turn_changed.then(|| {
                (
                    store.active_turn_id.clone(),
                    store.latest_turn_id.clone(),
                    store.evicted_active_turn.take(),
                    store.active_reasoning_item.clone(),
                )
            });
            (
                turn_state,
                store.replay_may_have_output || store.session.is_none() || store.turns != turns,
            )
        };
        self.install_selected_snapshot(child, session, turns, live)
            .await;
        let channel = self.ensure_thread_channel(child);
        match read_only {
            Some(ReadOnlyThread::Archived) => channel.mark_archived(),
            Some(ReadOnlyThread::ExternalWriter) => channel.mark_external_writer(),
            None => {}
        }
        {
            let mut store = channel.store.lock().await;
            if let Some((active, latest, evicted, reasoning)) = turn_state {
                store.active_turn_id = active;
                store.latest_turn_id = latest;
                store.evicted_active_turn = evicted;
                store.active_reasoning_item = reasoning;
            }
            if closed {
                store.clear_active_turn_id();
                store.active_reasoning_item = None;
                store.pending_interrupt_turn_id = None;
            }
            store.replay_may_have_output |= replay_output;
        }
        self.upsert_agent_picker_thread(child, None, None, closed);
        self.sync_agent_picker_thread(child);
        self.select_installed_side_locally(tui, server, child, live)
            .await
    }
}
