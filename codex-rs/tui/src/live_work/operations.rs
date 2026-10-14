//! A disposable projection of thread snapshots and runtime lifecycle notifications.
//! Nothing here executes work, owns a runtime, or changes transcript captures.

use super::Action;
use super::Key;
use super::Row;
use crate::code_mode_transcript::Admission;
use crate::code_mode_transcript::Source;
use crate::history_cell::code_mode_owner_label;
use crate::history_cell::insert_code_mode_fact;
use crate::history_cell::sanitize_user_text;
use codex_app_server_protocol::CodeModeEvent;
use codex_app_server_protocol::CodeModeItem;
use codex_app_server_protocol::CodeModeOwner;
use codex_app_server_protocol::CollabAgentTool;
use codex_app_server_protocol::CollabAgentToolCallStatus;
use codex_app_server_protocol::CommandExecutionStatus;
use codex_app_server_protocol::DynamicToolCallStatus;
use codex_app_server_protocol::McpToolCallStatus;
use codex_app_server_protocol::PatchApplyStatus;
use codex_app_server_protocol::ServerNotification;
use codex_app_server_protocol::ThreadItem;
use codex_app_server_protocol::Turn;
use codex_app_server_protocol::TurnStatus;
use codex_protocol::ThreadId;
use codex_protocol::models::ResponseItem;
use ratatui::text::Line;
use std::collections::BTreeMap;
use std::collections::HashSet;
use std::collections::VecDeque;
use std::collections::btree_map::Entry;
use std::time::Instant;

pub(crate) struct Operations {
    stage: Stage,
    active_turn: Option<String>,
    closed_turns: VecDeque<String>,
    capacity: usize,
    direct: BTreeMap<(String, String), Revision>,
    executions: BTreeMap<String, Execution>,
}

#[derive(Default)]
enum Stage {
    #[default]
    Unbound,
    Attached(ThreadId),
    Closed(ThreadId),
}

enum Pending {
    Item(Box<ThreadItem>),
    Call(String),
}

enum Revision {
    Pending { source: Source, work: Pending },
    Terminal(Instant),
}

impl Revision {
    fn admit(slot: &mut Self, work: Option<Pending>, source: Source) {
        if matches!(slot, Self::Terminal(_))
            || (source == Source::Historical
                && work.is_some()
                && matches!(
                    slot,
                    Self::Pending {
                        source: Source::Live,
                        ..
                    }
                ))
        {
            return;
        }
        *slot = work.map_or_else(
            || Self::Terminal(Instant::now()),
            |work| Self::Pending { source, work },
        );
    }

    fn command(&self) -> bool {
        matches!(self, Self::Pending { source: Source::Live, work: Pending::Item(item) }
            if matches!(item.as_ref(), ThreadItem::CommandExecution { .. }))
    }
}

struct Execution {
    owner: CodeModeOwner,
    facts: Vec<CodeModeItem>,
    children: BTreeMap<String, Revision>,
}

impl Operations {
    pub(crate) fn new(capacity: usize) -> Self {
        Self {
            stage: Stage::Unbound,
            active_turn: None,
            closed_turns: Default::default(),
            capacity,
            direct: BTreeMap::new(),
            executions: BTreeMap::new(),
        }
    }

    fn close_turn(&mut self, turn: &str) {
        if !self.closed_turns.iter().any(|closed| closed == turn) {
            self.closed_turns.push_back(turn.to_owned());
        }
        while self.closed_turns.len() > self.capacity {
            self.closed_turns.pop_front();
        }
    }

    fn trim(&mut self) {
        trim_revisions(&mut self.direct, self.capacity);
        for execution in self.executions.values_mut() {
            trim_revisions(&mut execution.children, self.capacity);
            let completed = execution
                .facts
                .iter()
                .filter_map(|fact| match &fact.event {
                    CodeModeEvent::WaitCompleted { call_id, .. } => Some(call_id.clone()),
                    _ => None,
                })
                .collect::<HashSet<_>>();
            let mut excess = completed.len().saturating_sub(self.capacity);
            execution.facts.retain(|fact| match &fact.event {
                CodeModeEvent::WaitStarted { call_id, .. } => !completed.contains(call_id),
                CodeModeEvent::WaitCompleted { .. } if excess > 0 => {
                    excess -= 1;
                    false
                }
                _ => true,
            });
        }
        let mut retired = self
            .executions
            .iter()
            .filter(|(_, execution)| {
                execution.finished() && !execution.children.values().any(Revision::command)
            })
            .map(|(id, execution)| (execution.owner.started_at_ms, id.clone()))
            .collect::<Vec<_>>();
        retired.sort_unstable();
        let excess = retired.len().saturating_sub(self.capacity);
        for (_, id) in retired.into_iter().take(excess) {
            self.executions.remove(&id);
        }
    }

    pub(crate) fn refresh(&mut self, thread_id: ThreadId, turns: &[Turn]) {
        if !matches!(self.stage, Stage::Attached(id) | Stage::Closed(id) if id == thread_id) {
            *self = Self::new(self.capacity);
        }
        self.stage = Stage::Attached(thread_id);
        self.active_turn = turns
            .iter()
            .rev()
            .find(|turn| turn.status == TurnStatus::InProgress)
            .map(|turn| turn.id.clone());
        for turn in turns
            .iter()
            .filter(|turn| turn.status != TurnStatus::InProgress)
        {
            self.close_turn(&turn.id);
        }
        self.direct
            .retain(|(turn, _), _| self.active_turn.as_ref() == Some(turn));

        // Reconcile terminal facts before older starts, without retaining the transcript's terminals.
        let finished = turns
            .iter()
            .flat_map(|turn| &turn.items)
            .filter_map(|item| {
                let ThreadItem::CodeModeOwned(owned) = item else {
                    return None;
                };
                (owned.owner.thread_id == thread_id
                    && matches!(owned.payload().as_ref(), ThreadItem::CodeMode(fact)
                    if matches!(fact.event, CodeModeEvent::Finished { .. })))
                .then_some((owned.owner.exec_call_id.clone(), owned))
            })
            .collect::<BTreeMap<_, _>>();
        let mut retired = finished.values().copied().collect::<Vec<_>>();
        retired.sort_unstable_by_key(|owned| owned.owner.started_at_ms);
        let recent = retired
            .into_iter()
            .rev()
            .take(self.capacity)
            .map(|owned| owned.owner.exec_call_id.clone())
            .collect::<HashSet<_>>();
        for (id, owned) in &finished {
            if recent.contains(id) || self.executions.contains_key(id) {
                self.owned_item(&owned.owner, owned.payload(), Source::Historical, None);
            }
        }
        for turn in turns {
            for item in &turn.items {
                if let ThreadItem::CodeModeOwned(owned) = item {
                    if owned.owner.thread_id == thread_id
                        && (!finished.contains_key(&owned.owner.exec_call_id)
                            || self.executions.get(&owned.owner.exec_call_id).is_some_and(
                                |execution| execution.children.values().any(Revision::command),
                            ))
                    {
                        self.owned_item(&owned.owner, owned.payload(), Source::Historical, None);
                    }
                } else if self.active_turn.as_ref() == Some(&turn.id) {
                    self.direct_item(&turn.id, item, Source::Historical, None);
                }
            }
        }
        self.trim();
    }

    pub(crate) fn notification(&mut self, notification: &ServerNotification) {
        let admission = match Admission::notification(notification, Source::Live) {
            Ok(admission) => admission,
            Err(error) => {
                tracing::warn!(%error, "invalid live-work Code Mode notification");
                return;
            }
        };
        let native = admission
            .as_ref()
            .map_or(notification, Admission::native_notification);
        let thread = match native {
            ServerNotification::TurnStarted(n) => &n.thread_id,
            ServerNotification::TurnCompleted(n) => &n.thread_id,
            ServerNotification::ItemStarted(n) => &n.thread_id,
            ServerNotification::ItemCompleted(n) => &n.thread_id,
            ServerNotification::RawResponseItemCompleted(n) => &n.thread_id,
            ServerNotification::StreamItemsDiscarded(n) => &n.thread_id,
            ServerNotification::ThreadClosed(n) => &n.thread_id,
            _ => return,
        };
        let Ok(thread_id) = ThreadId::from_string(thread) else {
            return;
        };
        match self.stage {
            Stage::Unbound => self.stage = Stage::Attached(thread_id),
            Stage::Attached(id) if id == thread_id => {}
            Stage::Closed(id)
                if id == thread_id
                    && matches!(native, ServerNotification::TurnStarted(n)
                    if !self.closed_turns.contains(&n.turn.id)) =>
            {
                self.stage = Stage::Attached(id);
            }
            _ => return,
        }
        if let Some(admission) = admission {
            match admission.native_notification() {
                ServerNotification::ItemStarted(n) => {
                    self.owned_item(&admission.owner, &n.item, Source::Live, Some(true));
                }
                ServerNotification::ItemCompleted(n) => {
                    self.owned_item(&admission.owner, &n.item, Source::Live, Some(false));
                }
                _ => {}
            }
            self.trim();
            return;
        }
        match native {
            ServerNotification::TurnStarted(n) if !self.closed_turns.contains(&n.turn.id) => {
                self.active_turn = Some(n.turn.id.clone());
                self.direct.retain(|(turn, _), _| turn == &n.turn.id);
                for item in &n.turn.items {
                    self.direct_item(&n.turn.id, item, Source::Historical, None);
                }
            }
            ServerNotification::TurnCompleted(n) => {
                self.close_turn(&n.turn.id);
                self.direct.retain(|(turn, _), _| turn != &n.turn.id);
                if self.active_turn.as_ref() == Some(&n.turn.id) {
                    self.active_turn = None;
                }
            }
            ServerNotification::ItemStarted(n) if self.accepts_turn(&n.turn_id) => {
                self.direct_item(&n.turn_id, &n.item, Source::Live, Some(true));
            }
            ServerNotification::ItemCompleted(n) => {
                self.direct_item(&n.turn_id, &n.item, Source::Live, Some(false));
            }
            ServerNotification::RawResponseItemCompleted(n) if self.accepts_turn(&n.turn_id) => {
                self.raw_call(&n.turn_id, &n.item);
            }
            ServerNotification::StreamItemsDiscarded(n) => {
                for id in &n.item_ids {
                    self.direct.insert(
                        (n.turn_id.clone(), id.clone()),
                        Revision::Terminal(Instant::now()),
                    );
                    for execution in self.executions.values_mut() {
                        if execution.children.contains_key(id) {
                            execution
                                .children
                                .insert(id.clone(), Revision::Terminal(Instant::now()));
                        }
                    }
                }
            }
            ServerNotification::ThreadClosed(_) => {
                self.stage = Stage::Closed(thread_id);
                self.active_turn = None;
                self.direct.clear();
                self.executions.clear();
            }
            _ => {}
        }
        self.trim();
    }

    fn accepts_turn(&self, turn: &str) -> bool {
        !self.closed_turns.iter().any(|closed| closed == turn)
            && self
                .active_turn
                .as_deref()
                .is_none_or(|active| active == turn)
    }

    fn direct_item(
        &mut self,
        turn: &str,
        item: &ThreadItem,
        source: Source,
        started: Option<bool>,
    ) {
        if let Some(call) = output_call(item) {
            finish_call(&mut self.direct, &(turn.to_owned(), call.to_owned()));
            return;
        }
        let Some(pending) = pending(item, started) else {
            return;
        };
        if pending && !self.accepts_turn(turn) {
            return;
        }
        admit(
            &mut self.direct,
            (turn.to_owned(), item.id().to_owned()),
            pending.then(|| Pending::Item(Box::new(item.clone()))),
            source,
        );
    }

    fn raw_call(&mut self, turn: &str, item: &ResponseItem) {
        match item {
            ResponseItem::FunctionCall {
                call_id,
                name,
                namespace,
                ..
            }
            | ResponseItem::CustomToolCall {
                call_id,
                name,
                namespace,
                ..
            } => {
                if self.executions.contains_key(call_id) {
                    return;
                }
                self.direct
                    .entry((turn.to_owned(), call_id.clone()))
                    .or_insert_with(|| Revision::Pending {
                        source: Source::Live,
                        work: Pending::Call(namespace.as_ref().map_or_else(
                            || name.clone(),
                            |namespace| format!("{namespace}.{name}"),
                        )),
                    });
            }
            ResponseItem::FunctionCallOutput {
                call_id: Some(call),
                ..
            }
            | ResponseItem::CustomToolCallOutput { call_id: call, .. } => {
                finish_call(&mut self.direct, &(turn.to_owned(), call.clone()));
            }
            _ => {}
        }
    }

    fn owned_item(
        &mut self,
        owner: &CodeModeOwner,
        item: &ThreadItem,
        source: Source,
        started: Option<bool>,
    ) {
        let execution = self
            .executions
            .entry(owner.exec_call_id.clone())
            .or_insert_with(|| Execution {
                owner: owner.clone(),
                facts: Vec::new(),
                children: BTreeMap::new(),
            });
        if execution.owner.started_at_ms != owner.started_at_ms
            || execution
                .owner
                .cell_id
                .as_ref()
                .zip(owner.cell_id.as_ref())
                .is_some_and(|(old, new)| old != new)
        {
            tracing::warn!(
                exec_call_id = owner.exec_call_id,
                "conflicting live-work execution owner"
            );
            return;
        }
        if execution.owner.cell_id.is_none() {
            execution.owner.cell_id = owner.cell_id.clone();
        }
        if let ThreadItem::CodeMode(fact) = item {
            if fact.exec_call_id != owner.exec_call_id
                || fact
                    .cell_id
                    .as_ref()
                    .zip(execution.owner.cell_id.as_ref())
                    .is_some_and(|(fact, owner)| fact != owner)
            {
                return;
            }
            if let CodeModeEvent::ToolCall { call_id } = &fact.event {
                execution
                    .children
                    .entry(call_id.clone())
                    .or_insert_with(|| Revision::Pending {
                        source,
                        work: Pending::Call("Tool call".to_owned()),
                    });
            }
            if let CodeModeEvent::WaitCompleted { call_id, .. } = &fact.event {
                finish_call(&mut execution.children, call_id);
            }
            if !matches!(fact.event, CodeModeEvent::ToolCall { .. }) {
                insert_code_mode_fact(&mut execution.facts, fact.clone());
            }
            if execution.finished() {
                for revision in execution.children.values_mut() {
                    if !revision.command() {
                        *revision = Revision::Terminal(Instant::now());
                    }
                }
            }
            // The typed execution supersedes the provider call, without deleting its capture.
            self.direct.retain(|(_, id), _| id != &owner.exec_call_id);
        } else if let Some(call) = output_call(item) {
            finish_call(&mut execution.children, &call.to_owned());
        } else if let Some(pending) = pending(item, started) {
            if execution.finished() && pending {
                return;
            }
            admit(
                &mut execution.children,
                item.id().to_owned(),
                pending.then(|| Pending::Item(Box::new(item.clone()))),
                source,
            );
        }
    }

    pub(crate) fn rows(&self, thread_id: ThreadId) -> Vec<Row> {
        if !matches!(self.stage, Stage::Attached(id) if id == thread_id) {
            return Vec::new();
        }
        let mut rows = self
            .direct
            .iter()
            .filter_map(|((turn, id), revision)| {
                if !self.accepts_turn(turn) {
                    return None;
                }
                revision_row(
                    thread_id,
                    Key::Operation {
                        thread_id,
                        id: id.clone(),
                    },
                    id,
                    revision,
                )
            })
            .collect::<Vec<_>>();
        rows.extend(
            self.executions
                .values()
                .flat_map(|execution| execution.rows()),
        );
        rows
    }
}

impl Execution {
    fn finished(&self) -> bool {
        self.facts
            .iter()
            .any(|fact| matches!(fact.event, CodeModeEvent::Finished { .. }))
    }

    fn rows(&self) -> Vec<Row> {
        let owner = &self.owner;
        let finished = self.finished();
        let mut children = self
            .children
            .iter()
            .filter_map(|(id, revision)| {
                revision_row(
                    owner.thread_id,
                    Key::CodeModeCall {
                        thread_id: owner.thread_id,
                        exec_call_id: owner.exec_call_id.clone(),
                        call_id: id.clone(),
                    },
                    id,
                    revision,
                )
            })
            .collect::<Vec<_>>();
        let waits = self
            .facts
            .iter()
            .filter_map(|fact| match &fact.event {
                CodeModeEvent::WaitStarted { call_id, target }
                    if !finished
                        && !self.facts.iter().any(|fact| {
                            matches!(&fact.event,
                    CodeModeEvent::WaitCompleted { call_id: completed, .. } if completed == call_id)
                        }) =>
                {
                    Some((call_id, target))
                }
                _ => None,
            })
            .collect::<Vec<_>>();
        children.retain(|child| {
            !matches!(&child.key, Key::CodeModeCall { call_id, .. }
            if waits.iter().any(|(waiting, _)| *waiting == call_id))
        });
        let running = !children.is_empty()
            || (!finished
                && self
                    .facts
                    .iter()
                    .any(|fact| matches!(fact.event, CodeModeEvent::Started { .. }))
                && !waits.iter().any(|(_, target)| target.is_some()));
        for (call_id, target) in waits {
            let target = target.as_ref().unwrap_or(owner);
            children.push(row(
                owner.thread_id,
                Key::CodeModeWait {
                    thread_id: owner.thread_id,
                    exec_call_id: owner.exec_call_id.clone(),
                    call_id: call_id.clone(),
                },
                call_id,
                format!("Waiting for ({})", code_mode_owner_label(target)),
                vec![line(format!(
                    "Agent {} · call {}",
                    target.thread_id, target.exec_call_id
                ))],
            ));
        }
        if !running {
            return children;
        }
        let mut row = row(
            owner.thread_id,
            Key::CodeMode {
                thread_id: owner.thread_id,
                exec_call_id: owner.exec_call_id.clone(),
            },
            &owner.exec_call_id,
            format!("Running ({})", code_mode_owner_label(owner)),
            vec![line(format!("Call {}", owner.exec_call_id))],
        );
        row.children = children;
        vec![row]
    }
}

fn admit<K: Ord>(items: &mut BTreeMap<K, Revision>, key: K, work: Option<Pending>, source: Source) {
    match items.entry(key) {
        Entry::Occupied(mut slot) => Revision::admit(slot.get_mut(), work, source),
        Entry::Vacant(slot) => {
            slot.insert(work.map_or_else(
                || Revision::Terminal(Instant::now()),
                |work| Revision::Pending { source, work },
            ));
        }
    }
}

fn trim_revisions<K: Ord + Clone>(items: &mut BTreeMap<K, Revision>, capacity: usize) {
    let mut retired = items
        .iter()
        .filter_map(|(key, revision)| match revision {
            Revision::Terminal(at) => Some((*at, key.clone())),
            _ => None,
        })
        .collect::<Vec<_>>();
    retired.sort_unstable();
    let excess = retired.len().saturating_sub(capacity);
    for (_, key) in retired.into_iter().take(excess) {
        items.remove(&key);
    }
}

fn finish_call<K: Ord + Clone>(items: &mut BTreeMap<K, Revision>, key: &K) {
    match items.entry(key.clone()) {
        Entry::Vacant(slot) => {
            slot.insert(Revision::Terminal(Instant::now()));
        }
        Entry::Occupied(mut slot) => {
            if matches!(
                slot.get(),
                Revision::Pending {
                    work: Pending::Call(_),
                    ..
                }
            ) {
                slot.insert(Revision::Terminal(Instant::now()));
            }
        }
    }
}

fn output_call(item: &ThreadItem) -> Option<&str> {
    match item {
        ThreadItem::FunctionCallOutput { call_id, .. } => call_id.as_deref(),
        ThreadItem::ToolOutputTokens { call_id, .. } => Some(call_id),
        _ => None,
    }
}

fn pending(item: &ThreadItem, started: Option<bool>) -> Option<bool> {
    Some(match item {
        ThreadItem::CommandExecution { status, .. } => {
            *status == CommandExecutionStatus::InProgress
        }
        ThreadItem::FileChange { status, .. } => *status == PatchApplyStatus::InProgress,
        ThreadItem::McpToolCall { status, .. } => *status == McpToolCallStatus::InProgress,
        ThreadItem::DynamicToolCall { status, .. } => *status == DynamicToolCallStatus::InProgress,
        ThreadItem::CollabAgentToolCall { status, .. } => {
            *status == CollabAgentToolCallStatus::InProgress
        }
        ThreadItem::WebSearch(item) => started.unwrap_or(item.action.is_none()),
        ThreadItem::ImageGeneration(item) => {
            started.unwrap_or(item.status.is_empty() || item.status == "in_progress")
        }
        ThreadItem::Sleep(_)
        | ThreadItem::ImageView { .. }
        | ThreadItem::ContextCompaction { .. } => started?,
        _ => return None,
    })
}

fn revision_row(thread: ThreadId, key: Key, id: &str, revision: &Revision) -> Option<Row> {
    let Revision::Pending { work, .. } = revision else {
        return None;
    };
    let mut details = vec![line(format!("Call {id}"))];
    let summary = match work {
        Pending::Call(name) => name.clone(),
        Pending::Item(item) => match item.as_ref() {
            ThreadItem::CommandExecution { command, cwd, .. } => {
                details.push(line(format!("Directory {cwd}")));
                format!("Command · {}", command.lines().next().unwrap_or_default())
            }
            ThreadItem::McpToolCall { server, tool, .. } => format!("{server}.{tool}"),
            ThreadItem::DynamicToolCall {
                namespace, tool, ..
            } => namespace
                .as_ref()
                .map_or_else(|| tool.clone(), |namespace| format!("{namespace}.{tool}")),
            ThreadItem::FileChange { changes, .. } => {
                details.extend(changes.iter().map(|change| line(change.path.clone())));
                "Editing files".to_owned()
            }
            ThreadItem::CollabAgentToolCall {
                tool,
                receiver_thread_ids,
                ..
            } => {
                details.extend(
                    receiver_thread_ids
                        .iter()
                        .map(|id| line(format!("Agent {id}"))),
                );
                match tool {
                    CollabAgentTool::Wait => "Waiting for agent updates".to_owned(),
                    _ => format!("Agent operation · {tool:?}"),
                }
            }
            ThreadItem::WebSearch(item) => {
                if item.query.is_empty() {
                    "Searching the web".to_owned()
                } else {
                    format!("Web search · {}", item.query)
                }
            }
            ThreadItem::ImageGeneration(_) => "Generating an image".to_owned(),
            ThreadItem::ImageView { path, .. } => format!("Viewing image · {path}"),
            ThreadItem::Sleep(item) => format!("Sleeping · {} ms", item.duration_ms),
            ThreadItem::ContextCompaction { .. } => "Compacting context".to_owned(),
            _ => return None,
        },
    };
    Some(row(thread, key, id, summary, details))
}

fn row(
    thread_id: ThreadId,
    key: Key,
    item_id: &str,
    summary: String,
    details: Vec<Line<'static>>,
) -> Row {
    Row {
        key,
        summary: line(summary),
        details,
        children: Vec::new(),
        actions: vec![Action::RevealCall {
            thread_id,
            item_id: item_id.to_owned(),
        }],
    }
}

fn line(text: String) -> Line<'static> {
    Line::from(sanitize_user_text(text.into()).into_owned())
}
