//! One scoped reducer for live notifications and independently hydrated history items.
//! Membership is carried by the envelope, never inferred from chronology or output text.

use crate::history_cell::CodeModeCell;
use crate::history_cell::CodeModeFactCell;
use crate::history_cell::CodeModeWaitCell;
use crate::history_cell::HistoryCell;
use codex_app_server_protocol::CodeModeEvent;
use codex_app_server_protocol::CodeModeOwned;
use codex_app_server_protocol::CodeModeOwner;
use codex_app_server_protocol::ItemCompletedNotification;
use codex_app_server_protocol::ServerNotification;
use codex_app_server_protocol::ThreadItem;
use codex_protocol::ThreadId;
use codex_utils_absolute_path::AbsolutePathBuf;
use std::collections::HashMap;
use std::fmt::Display;
use std::fmt::Formatter;
use std::sync::Arc;

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
    MissingDescriptor,
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
            Self::MissingDescriptor => "missing Code Mode descriptor",
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
                let ThreadItem::CodeModeOwned(owned) = &started.item else {
                    return Ok(None);
                };
                let mut native = started.clone();
                native.item = owned.payload().as_ref().clone();
                (owned.owner.clone(), ServerNotification::ItemStarted(native))
            }
            ServerNotification::ItemCompleted(completed) => {
                let ThreadItem::CodeModeOwned(owned) = &completed.item else {
                    return Ok(None);
                };
                let mut native = completed.clone();
                native.item = owned.payload().as_ref().clone();
                (
                    owned.owner.clone(),
                    ServerNotification::ItemCompleted(native),
                )
            }
            _ => return Ok(None),
        };
        let thread = match &native {
            ServerNotification::ItemStarted(item) => {
                if matches!(item.item, ThreadItem::CodeModeOwned(_)) {
                    return Err(AdmissionFault::NestedItem);
                }
                &item.thread_id
            }
            ServerNotification::ItemCompleted(item) => {
                if matches!(item.item, ThreadItem::CodeModeOwned(_)) {
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

    pub(crate) fn item(owned: CodeModeOwned<Box<ThreadItem>>, turn_id: String) -> Self {
        let (owner, item) = owned.into_parts();
        let notification = if matches!(
            &*item,
            ThreadItem::CommandExecution {
                status: codex_app_server_protocol::CommandExecutionStatus::InProgress,
                ..
            }
        ) {
            ServerNotification::ItemStarted(codex_app_server_protocol::ItemStartedNotification {
                thread_id: owner.thread_id.to_string(),
                turn_id,
                item: *item,
                started_at_ms: owner.started_at_ms,
            })
        } else {
            ServerNotification::ItemCompleted(ItemCompletedNotification {
                thread_id: owner.thread_id.to_string(),
                turn_id,
                item: *item,
                completed_at_ms: owner.started_at_ms,
            })
        };
        Self {
            notification,
            owner,
            source: Source::Historical,
        }
    }
}

#[derive(Default)]
pub(crate) struct Reducers(HashMap<ThreadId, HashMap<(ThreadId, String), Block>>);

struct Block {
    cell: CodeModeCell,
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
            .filter_map(|cell| cell.owner.as_ref())
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
        let owner = incoming
            .owner
            .as_ref()
            .ok_or(AdmissionFault::UnownedHydration)?;
        let block = self
            .0
            .entry(display)
            .or_default()
            .entry((owner.thread_id, owner.exec_call_id.clone()))
            .or_insert_with(|| Block {
                cell: incoming.clone(),
            });
        let retained = block
            .cell
            .owner
            .as_mut()
            .ok_or(AdmissionFault::MissingDescriptor)?;
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
            retained.cell_id = owner.cell_id.clone();
        }
        block.cell.hydrate(incoming);
        Ok(Arc::new(block.cell.clone()))
    }

    pub(crate) fn admit(
        &mut self,
        display: ThreadId,
        admission: Admission,
        cwd: &AbsolutePathBuf,
        animations: bool,
        metadata: &dyn Fn(ThreadId) -> crate::multi_agents::AgentMetadata,
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
            .or_insert_with(|| Block {
                cell: CodeModeCell::owned(owner.clone(), animations),
            });
        let descriptor = block
            .cell
            .owner
            .as_mut()
            .ok_or(AdmissionFault::MissingDescriptor)?;
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
        let active_before = block.cell.is_active();
        let mut wait = None;
        let mut tokens = None;
        match notification {
            ServerNotification::ItemStarted(started) => {
                block.item(started.item, source, cwd, &mut wait, &mut tokens, metadata)?;
            }
            ServerNotification::ItemCompleted(completed) => {
                block.item(
                    completed.item,
                    source,
                    cwd,
                    &mut wait,
                    &mut tokens,
                    metadata,
                )?;
            }
            ServerNotification::CommandExecutionOutputDelta(delta) => {
                if let Some(item @ ThreadItem::CommandExecution { .. }) =
                    block.cell.pending_children.get_mut(&delta.item_id)
                {
                    if let ThreadItem::CommandExecution {
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
                    );
                    block
                        .cell
                        .child_versions
                        .insert(id.clone(), (source, false));
                    block.cell.admit_children(id, cells);
                }
            }
            ServerNotification::FileChangePatchUpdated(updated) => {
                if let Some(item @ ThreadItem::FileChange { .. }) =
                    block.cell.pending_children.get_mut(&updated.item_id)
                {
                    if let ThreadItem::FileChange { changes, .. } = item {
                        *changes = updated.changes;
                    }
                    let id = item.id().to_owned();
                    let cells = crate::thread_transcript::scoped_item_cells(
                        item.clone(),
                        cwd,
                        source,
                        metadata,
                    );
                    block
                        .cell
                        .child_versions
                        .insert(id.clone(), (source, false));
                    block.cell.admit_children(id, cells);
                }
            }
            ServerNotification::TerminalInteraction(interaction) => {
                let command = block
                    .cell
                    .pending_children
                    .values()
                    .find_map(|item| match item {
                        ThreadItem::CommandExecution {
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
                    block.cell.admit_children(
                        // Replay starts with a fresh reducer; page hydration preserves child IDs.
                        format!(
                            "terminal-interaction:{}:{}",
                            interaction.item_id,
                            block.cell.children.len()
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
            opens_live: !active_before && block.cell.is_active(),
            block: Arc::new(block.cell.clone()),
            wait,
            tokens,
        })
    }
}

impl Block {
    fn item(
        &mut self,
        item: ThreadItem,
        source: Source,
        cwd: &AbsolutePathBuf,
        wait: &mut Option<Arc<dyn HistoryCell>>,
        tokens: &mut Option<TokenBill>,
        metadata: &dyn Fn(ThreadId) -> crate::multi_agents::AgentMetadata,
    ) -> Result<(), AdmissionFault> {
        match item {
            ThreadItem::CodeModeOwned(_) => return Err(AdmissionFault::NestedItem),
            ThreadItem::CodeMode(fact) => {
                let owner = self
                    .cell
                    .owner
                    .as_ref()
                    .ok_or(AdmissionFault::MissingDescriptor)?;
                if fact.exec_call_id != owner.exec_call_id
                    || fact
                        .cell_id
                        .as_ref()
                        .zip(owner.cell_id.as_ref())
                        .is_some_and(|(fact, owner)| fact != owner)
                {
                    return Err(AdmissionFault::ConflictingFact);
                }
                if matches!(
                    fact.event,
                    CodeModeEvent::WaitStarted { .. } | CodeModeEvent::WaitCompleted { .. }
                ) {
                    *wait = Some(Arc::new(CodeModeWaitCell(
                        CodeModeFactCell(fact.clone(), false),
                        owner.clone(),
                    )));
                }
                self.cell.admit_fact(fact, source == Source::Live);
            }
            ThreadItem::ToolOutputTokens {
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
                if let Some(ThreadItem::CollabAgentToolCall {
                    prompt: old_prompt,
                    model: old_model,
                    reasoning_effort: old_effort,
                    ..
                }) = self.cell.pending_children.get(&id)
                    && let ThreadItem::CollabAgentToolCall {
                        prompt,
                        model,
                        reasoning_effort,
                        ..
                    } = &mut item
                {
                    if prompt.is_none() {
                        *prompt = old_prompt.clone();
                    }
                    if model.is_none() {
                        *model = old_model.clone();
                    }
                    if reasoning_effort.is_none() {
                        *reasoning_effort = old_effort.clone();
                    }
                }
                let pending = matches!(
                    &item,
                    ThreadItem::CommandExecution {
                        status: codex_app_server_protocol::CommandExecutionStatus::InProgress,
                        ..
                    } | ThreadItem::McpToolCall {
                        status: codex_app_server_protocol::McpToolCallStatus::InProgress,
                        ..
                    } | ThreadItem::FileChange {
                        status: codex_app_server_protocol::PatchApplyStatus::InProgress,
                        ..
                    } | ThreadItem::CollabAgentToolCall {
                        status: codex_app_server_protocol::CollabAgentToolCallStatus::InProgress,
                        ..
                    }
                );
                if self
                    .cell
                    .child_versions
                    .get(&id)
                    .is_some_and(|(previous, terminal)| {
                        (*terminal && pending)
                            || (*previous == Source::Live
                                && source == Source::Historical
                                && (*terminal || pending))
                    })
                {
                    return Ok(());
                }
                self.cell
                    .child_versions
                    .insert(id.clone(), (source, !pending));
                if pending {
                    self.cell.pending_children.insert(id.clone(), item.clone());
                } else {
                    self.cell.pending_children.remove(&id);
                }
                self.cell.admit_children(
                    id,
                    crate::thread_transcript::scoped_item_cells(item, cwd, source, metadata),
                );
            }
        }
        Ok(())
    }
}
