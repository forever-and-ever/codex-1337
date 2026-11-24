//! One scoped reducer for live notifications and independently hydrated history items.
//! Membership is carried by the envelope, never inferred from chronology or output text.

use crate::history_cell::CodeModeCell;
use crate::history_cell::CodeModeFactCell;
use crate::history_cell::CodeModeWaitCell;
use crate::history_cell::HistoryCell;
use crate::history_cell::ToolOutputCell;
use crate::history_cell::insert_code_mode_fact;
use codex_app_server_protocol::CodeModeEvent;
use codex_app_server_protocol::CodeModeItem;
use codex_app_server_protocol::CodeModeOwned;
use codex_app_server_protocol::CodeModeOwner;
use codex_app_server_protocol::ItemCompletedNotification;
use codex_app_server_protocol::ServerNotification;
use codex_app_server_protocol::ThreadItemPayload;
use codex_protocol::ThreadId;
use std::collections::HashMap;
use std::fmt::Display;
use std::fmt::Formatter;
use std::path::Path;
use std::sync::Arc;
use std::time::Instant;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum Source {
    Live,
    Historical,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum AdmissionFault {
    NestedItem,
    IneligibleNotification,
    ConflictingThread,
    UnownedHydration,
    ConflictingHydration,
    ConflictingExecutionDescriptor,
    UnvalidatedNotification,
    ConflictingFact,
}

impl Display for AdmissionFault {
    fn fmt(&self, f: &mut Formatter<'_>) -> std::fmt::Result {
        f.write_str(match self {
            Self::NestedItem => "nested Code Mode item",
            Self::IneligibleNotification => "ineligible Code Mode notification",
            Self::ConflictingThread => "conflicting Code Mode thread",
            Self::UnownedHydration => "unowned Code Mode hydration",
            Self::ConflictingHydration => "conflicting Code Mode hydration",
            Self::ConflictingExecutionDescriptor => "conflicting Code Mode execution descriptor",
            Self::UnvalidatedNotification => "unvalidated Code Mode notification",
            Self::ConflictingFact => "conflicting Code Mode fact",
        })
    }
}

#[derive(Debug)]
pub(crate) struct Admission {
    pub(crate) owner: CodeModeOwner,
    notification: ServerNotification,
    source: Source,
}

impl Admission {
    pub(crate) fn native_notification(&self) -> &ServerNotification {
        &self.notification
    }
    /// Normalize both legal representations. Cross-enum nesting and thread conflicts fail closed.
    pub(crate) fn notification(
        notification: &ServerNotification,
        source: Source,
    ) -> Result<Option<Self>, AdmissionFault> {
        let (owner, native) = match notification {
            ServerNotification::CodeModeOwned(owned) => {
                (owned.owner.clone(), owned.payload().as_ref().clone())
            }
            ServerNotification::ItemStarted(started) => {
                let ThreadItemPayload::CodeModeOwned(owned) = &started.item.payload else {
                    return Ok(None);
                };
                let mut native = started.clone();
                native.item.payload = owned.payload().as_ref().clone();
                (owned.owner.clone(), ServerNotification::ItemStarted(native))
            }
            ServerNotification::ItemCompleted(completed) => {
                let ThreadItemPayload::CodeModeOwned(owned) = &completed.item.payload else {
                    return Ok(None);
                };
                let mut native = completed.clone();
                native.item.payload = owned.payload().as_ref().clone();
                (
                    owned.owner.clone(),
                    ServerNotification::ItemCompleted(native),
                )
            }
            _ => return Ok(None),
        };
        let thread = match &native {
            ServerNotification::ItemStarted(item) => {
                if matches!(item.item.payload, ThreadItemPayload::CodeModeOwned(_)) {
                    return Err(AdmissionFault::NestedItem);
                }
                &item.thread_id
            }
            ServerNotification::ItemCompleted(item) => {
                if matches!(item.item.payload, ThreadItemPayload::CodeModeOwned(_)) {
                    return Err(AdmissionFault::NestedItem);
                }
                &item.thread_id
            }
            ServerNotification::CommandExecutionOutputDelta(item) => &item.thread_id,
            ServerNotification::TerminalInteraction(item) => &item.thread_id,
            ServerNotification::FileChangeOutputDelta(item) => &item.thread_id,
            ServerNotification::FileChangePatchUpdated(item) => &item.thread_id,
            ServerNotification::McpToolCallProgress(item) => &item.thread_id,
            _ => return Err(AdmissionFault::IneligibleNotification),
        };
        if *thread != owner.thread_id.to_string() {
            return Err(AdmissionFault::ConflictingThread);
        }
        Ok(Some(Self {
            owner,
            notification: native,
            source,
        }))
    }

    pub(crate) fn item(owned: CodeModeOwned<Box<ThreadItemPayload>>, turn_id: String) -> Self {
        let (owner, item) = owned.into_parts();
        let notification = if matches!(
            &*item,
            ThreadItemPayload::CommandExecution {
                status: codex_app_server_protocol::CommandExecutionStatus::InProgress,
                ..
            }
        ) {
            ServerNotification::ItemStarted(
                codex_app_server_protocol::ItemStartedNotification::new(
                    owner.thread_id.to_string(),
                    turn_id,
                    *item,
                    owner.started_at_ms,
                ),
            )
        } else {
            ServerNotification::ItemCompleted(ItemCompletedNotification::new(
                owner.thread_id.to_string(),
                turn_id,
                *item,
                None,
                owner.started_at_ms,
            ))
        };
        Self {
            notification,
            owner,
            source: Source::Historical,
        }
    }
}

#[derive(Default)]
pub(crate) struct Reducers(HashMap<ThreadId, HashMap<(ThreadId, String), ScopedCodeMode>>);

#[derive(Debug, Clone)]
pub(crate) struct ScopedCodeMode {
    owner: CodeModeOwner,
    facts: Vec<CodeModeItem>,
    live: bool,
    children: Vec<(String, Vec<Arc<dyn HistoryCell>>)>,
    revisions: HashMap<String, ChildRevision>,
    rendered_members: Vec<Arc<dyn HistoryCell>>,
    start_time: Instant,
    animations_enabled: bool,
}

#[derive(Debug, Clone)]
enum ChildRevision {
    Pending {
        source: Source,
        item: Box<ThreadItemPayload>,
    },
    Terminal {
        source: Source,
    },
}

impl ChildRevision {
    fn source(&self) -> Source {
        match self {
            Self::Pending { source, .. } | Self::Terminal { source } => *source,
        }
    }

    fn terminal(&self) -> bool {
        matches!(self, Self::Terminal { .. })
    }

    fn pending(&self) -> Option<&ThreadItemPayload> {
        match self {
            Self::Pending { item, .. } => Some(item),
            Self::Terminal { .. } => None,
        }
    }
}

#[derive(Debug)]
pub(crate) struct TokenBill {
    pub(crate) item_id: String,
    pub(crate) call_id: String,
    pub(crate) tokens: Option<u64>,
}

#[derive(Debug)]
pub(crate) struct Projection {
    pub(crate) opens_live: bool,
    pub(crate) block: Arc<dyn HistoryCell>,
    pub(crate) wait: Option<Arc<dyn HistoryCell>>,
    pub(crate) tokens: Option<TokenBill>,
}

impl Reducers {
    pub(crate) fn retain(&mut self, cells: &[Arc<dyn HistoryCell>]) {
        let retained = cells
            .iter()
            .filter_map(|cell| cell.as_any().downcast_ref::<CodeModeCell>())
            .filter_map(CodeModeCell::owner)
            .map(|owner| (owner.thread_id, owner.exec_call_id.clone()))
            .collect::<std::collections::HashSet<_>>();
        self.0.retain(|_, bank| {
            bank.retain(|key, _| retained.contains(key));
            !bank.is_empty()
        });
    }

    pub(crate) fn hydrate(
        &mut self,
        display: ThreadId,
        incoming: &CodeModeCell,
    ) -> Result<Arc<dyn HistoryCell>, AdmissionFault> {
        let incoming = incoming.scoped().ok_or(AdmissionFault::UnownedHydration)?;
        let owner = &incoming.owner;
        let block = self
            .0
            .entry(display)
            .or_default()
            .entry((owner.thread_id, owner.exec_call_id.clone()))
            .or_insert_with(|| incoming.clone());
        let retained = &mut block.owner;
        if retained.started_at_ms != owner.started_at_ms
            || retained
                .cell_id
                .as_ref()
                .zip(owner.cell_id.as_ref())
                .is_some_and(|(old, new)| old != new)
        {
            return Err(AdmissionFault::ConflictingHydration);
        }
        if retained.cell_id.is_none() {
            retained.cell_id.clone_from(&owner.cell_id);
        }
        block.hydrate(incoming);
        Ok(Arc::new(CodeModeCell::Scoped(block.clone())))
    }

    pub(crate) fn admit(
        &mut self,
        display: ThreadId,
        admission: Admission,
        cwd: &Path,
        animations: bool,
        metadata: &dyn Fn(ThreadId) -> crate::multi_agents::AgentMetadata,
        image_origin: crate::history_cell::ImagePathOrigin,
    ) -> Result<Projection, AdmissionFault> {
        let Admission {
            owner,
            notification,
            source,
        } = admission;
        let block = self
            .0
            .entry(display)
            .or_default()
            .entry((owner.thread_id, owner.exec_call_id.clone()))
            .or_insert_with(|| ScopedCodeMode::new(owner.clone(), animations));
        let descriptor = &mut block.owner;
        if descriptor.started_at_ms != owner.started_at_ms
            || descriptor
                .cell_id
                .as_ref()
                .zip(owner.cell_id.as_ref())
                .is_some_and(|(old, new)| old != new)
        {
            return Err(AdmissionFault::ConflictingExecutionDescriptor);
        }
        if descriptor.cell_id.is_none() {
            descriptor.cell_id = owner.cell_id;
        }
        let active_before = block.is_active();
        let mut wait = None;
        let mut tokens = None;
        match notification {
            ServerNotification::ItemStarted(started) => {
                block.item(
                    started.item.payload,
                    source,
                    cwd,
                    &mut wait,
                    &mut tokens,
                    metadata,
                    image_origin,
                )?;
            }
            ServerNotification::ItemCompleted(completed) => {
                block.item(
                    completed.item.payload,
                    source,
                    cwd,
                    &mut wait,
                    &mut tokens,
                    metadata,
                    image_origin,
                )?;
            }
            ServerNotification::CommandExecutionOutputDelta(delta) => {
                if let Some(ChildRevision::Pending {
                    source: previous,
                    item,
                }) = block.revisions.get_mut(&delta.item_id)
                    && let item @ ThreadItemPayload::CommandExecution { .. } = item.as_mut()
                {
                    if let ThreadItemPayload::CommandExecution {
                        aggregated_output, ..
                    } = item
                    {
                        aggregated_output
                            .get_or_insert_default()
                            .push_str(&delta.delta);
                    }
                    let id = item.id().to_owned();
                    let cells = crate::thread_transcript::scoped_item_cells(
                        item.clone(),
                        cwd,
                        source,
                        metadata,
                        image_origin,
                    );
                    *previous = source;
                    block.admit_children(id, cells);
                }
            }
            ServerNotification::FileChangePatchUpdated(updated) => {
                if let Some(ChildRevision::Pending {
                    source: previous,
                    item,
                }) = block.revisions.get_mut(&updated.item_id)
                    && let item @ ThreadItemPayload::FileChange { .. } = item.as_mut()
                {
                    if let ThreadItemPayload::FileChange { changes, .. } = item {
                        *changes = updated.changes;
                    }
                    let id = item.id().to_owned();
                    let cells = crate::thread_transcript::scoped_item_cells(
                        item.clone(),
                        cwd,
                        source,
                        metadata,
                        image_origin,
                    );
                    *previous = source;
                    block.admit_children(id, cells);
                }
            }
            ServerNotification::TerminalInteraction(interaction) => {
                let command = block
                    .revisions
                    .values()
                    .filter_map(ChildRevision::pending)
                    .find_map(|item| match item {
                        ThreadItemPayload::CommandExecution {
                            command,
                            process_id: Some(process),
                            ..
                        } if process == &interaction.process_id => {
                            Some(crate::exec_command::strip_bash_lc_and_escape(
                                &crate::exec_command::split_command_string(command),
                            ))
                        }
                        _ => None,
                    });
                if !interaction.stdin.is_empty() || command.is_some() {
                    block.admit_children(
                        // Replay starts with a fresh reducer; page hydration preserves child IDs.
                        format!(
                            "terminal-interaction:{}:{}",
                            interaction.item_id,
                            block.children.len()
                        ),
                        vec![Arc::new(crate::history_cell::new_unified_exec_interaction(
                            command,
                            interaction.stdin,
                        ))],
                    );
                }
            }
            // These native updates do not add independently captured model output.
            // Process/approval/roster observation remains outside this renderer.
            ServerNotification::FileChangeOutputDelta(_)
            | ServerNotification::McpToolCallProgress(_) => {}
            _ => return Err(AdmissionFault::UnvalidatedNotification),
        }
        Ok(Projection {
            opens_live: !active_before && block.is_active(),
            block: Arc::new(CodeModeCell::Scoped(block.clone())),
            wait,
            tokens,
        })
    }
}

impl ScopedCodeMode {
    pub(crate) fn new(owner: CodeModeOwner, animations_enabled: bool) -> Self {
        Self {
            owner,
            facts: Vec::new(),
            live: false,
            children: Vec::new(),
            revisions: HashMap::new(),
            rendered_members: Vec::new(),
            start_time: Instant::now(),
            animations_enabled,
        }
    }

    pub(crate) fn owner(&self) -> &CodeModeOwner {
        &self.owner
    }

    pub(crate) fn facts(&self) -> &[CodeModeItem] {
        &self.facts
    }

    pub(crate) fn members(&self) -> &[Arc<dyn HistoryCell>] {
        &self.rendered_members
    }

    pub(crate) fn start_time(&self) -> Instant {
        self.start_time
    }

    pub(crate) fn animations_enabled(&self) -> bool {
        self.animations_enabled
    }

    pub(crate) fn live(&self) -> bool {
        self.live
    }

    pub(crate) fn has_live_command(&self) -> bool {
        self.revisions.values().any(|revision| {
            matches!(
                revision,
                ChildRevision::Pending {
                    source: Source::Live,
                    item,
                } if matches!(item.as_ref(), ThreadItemPayload::CommandExecution { .. })
            )
        })
    }

    fn is_active(&self) -> bool {
        (self.live
            && !self
                .facts
                .iter()
                .any(|fact| matches!(fact.event, CodeModeEvent::Finished { .. })))
            || self.has_live_command()
    }

    fn admit_children(&mut self, id: String, cells: Vec<Arc<dyn HistoryCell>>) {
        if let Some((_, rows)) = self.children.iter_mut().find(|(old, _)| *old == id) {
            *rows = cells;
        } else {
            self.children.push((id, cells));
        }
        self.refresh_children();
    }

    fn hydrate(&mut self, older: &Self) {
        for fact in &older.facts {
            insert_code_mode_fact(&mut self.facts, fact.clone());
        }
        let newer = std::mem::take(&mut self.children);
        self.children.clone_from(&older.children);
        for (id, cells) in newer {
            if let Some((_, rows)) = self.children.iter_mut().find(|(old, _)| *old == id) {
                *rows = cells;
            } else {
                self.children.push((id, cells));
            }
        }
        self.refresh_children();
        for (id, revision) in &older.revisions {
            self.revisions
                .entry(id.clone())
                .or_insert_with(|| revision.clone());
        }
        self.facts.sort_by_key(|fact| fact.created_at);
    }

    fn refresh_children(&mut self) {
        self.rendered_members = self
            .children
            .iter()
            .flat_map(|(_, rows)| rows.iter().cloned())
            .collect();
        self.rendered_members
            .sort_by_key(|cell| cell.as_any().is::<ToolOutputCell>());
    }

    fn item(
        &mut self,
        item: ThreadItemPayload,
        source: Source,
        cwd: &Path,
        wait: &mut Option<Arc<dyn HistoryCell>>,
        tokens: &mut Option<TokenBill>,
        metadata: &dyn Fn(ThreadId) -> crate::multi_agents::AgentMetadata,
        image_origin: crate::history_cell::ImagePathOrigin,
    ) -> Result<(), AdmissionFault> {
        match item {
            ThreadItemPayload::CodeModeOwned(_) => return Err(AdmissionFault::NestedItem),
            ThreadItemPayload::CodeMode(fact) => {
                let owner = &self.owner;
                if fact.exec_call_id != owner.exec_call_id
                    || fact
                        .cell_id
                        .as_ref()
                        .zip(owner.cell_id.as_ref())
                        .is_some_and(|(fact, owner)| fact != owner)
                {
                    return Err(AdmissionFault::ConflictingFact);
                }
                if match &fact.event {
                    CodeModeEvent::WaitStarted { target, .. }
                    | CodeModeEvent::WaitCompleted { target, .. } => target.is_none(),
                    _ => false,
                } {
                    *wait = Some(Arc::new(CodeModeWaitCell(
                        CodeModeFactCell(fact.clone(), false),
                        owner.clone(),
                    )));
                }
                if source == Source::Live && matches!(fact.event, CodeModeEvent::Started { .. }) {
                    self.live = true;
                }
                insert_code_mode_fact(&mut self.facts, fact);
            }
            ThreadItemPayload::ToolOutputTokens {
                id,
                call_id,
                tokens: count,
                ..
            } => {
                *tokens = Some(TokenBill {
                    item_id: id,
                    call_id,
                    tokens: count,
                });
            }
            mut item => {
                let id = item.id().to_owned();
                if let Some(ThreadItemPayload::CollabAgentToolCall {
                    prompt: old_prompt,
                    model: old_model,
                    reasoning_effort: old_effort,
                    ..
                }) = self.revisions.get(&id).and_then(ChildRevision::pending)
                    && let ThreadItemPayload::CollabAgentToolCall {
                        prompt,
                        model,
                        reasoning_effort,
                        ..
                    } = &mut item
                {
                    if prompt.is_none() {
                        prompt.clone_from(old_prompt);
                    }
                    if model.is_none() {
                        model.clone_from(old_model);
                    }
                    if reasoning_effort.is_none() {
                        reasoning_effort.clone_from(old_effort);
                    }
                }
                let pending = matches!(
                    &item,
                    ThreadItemPayload::CommandExecution {
                        status: codex_app_server_protocol::CommandExecutionStatus::InProgress,
                        ..
                    } | ThreadItemPayload::McpToolCall {
                        status: codex_app_server_protocol::McpToolCallStatus::InProgress,
                        ..
                    } | ThreadItemPayload::FileChange {
                        status: codex_app_server_protocol::PatchApplyStatus::InProgress,
                        ..
                    } | ThreadItemPayload::CollabAgentToolCall {
                        status: codex_app_server_protocol::CollabAgentToolCallStatus::InProgress,
                        ..
                    }
                );
                if self.revisions.get(&id).is_some_and(|previous| {
                    (previous.terminal() && pending)
                        || (previous.source() == Source::Live
                            && source == Source::Historical
                            && (previous.terminal() || pending))
                }) {
                    return Ok(());
                }
                self.revisions.insert(
                    id.clone(),
                    if pending {
                        ChildRevision::Pending {
                            source,
                            item: Box::new(item.clone()),
                        }
                    } else {
                        ChildRevision::Terminal { source }
                    },
                );
                self.admit_children(
                    id,
                    crate::thread_transcript::scoped_item_cells(
                        item,
                        cwd,
                        source,
                        metadata,
                        image_origin,
                    ),
                );
            }
        }
        Ok(())
    }
}
