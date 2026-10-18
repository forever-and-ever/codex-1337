//! Disposable Work projection: one entry per item identity within the attached thread.
//! Custody belongs to a turn or a Code Mode execution; owned events re-parent, never duplicate.
//! Item terminals end either custody, owner terminals reap their members, and refresh rebuilds
//! pending state. Nothing here executes work, owns a runtime, or changes transcript captures.

use super::Action;
use super::Key;
use super::Row;
use crate::code_mode_transcript::Admission;
use crate::code_mode_transcript::Source;
use crate::exec_command::split_command_string;
use crate::exec_command::strip_bash_lc_and_escape;
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

#[derive(Debug)]
pub(crate) struct Operations {
    stage: Stage,
    active_turn: Option<String>,
    closed_turns: VecDeque<String>,
    capacity: usize,
    items: BTreeMap<String, Revision>,
    executions: BTreeMap<String, Execution>,
}

#[derive(Debug, Default)]
enum Stage {
    #[default]
    Unbound,
    Attached(ThreadId),
    Closed(ThreadId),
}

#[derive(Debug)]
enum Pending {
    Item(Box<ThreadItem>),
    Call(String),
}

#[derive(Debug, PartialEq, Eq)]
enum Custody {
    Turn(String),
    Execution(String),
}

#[derive(Debug)]
enum Revision {
    Pending {
        custody: Custody,
        source: Source,
        work: Pending,
    },
    Terminal(Instant),
}

impl Revision {
    fn admit(slot: &mut Self, work: Option<Pending>, source: Source, custody: Custody) {
        if matches!(slot, Self::Terminal(_)) {
            return;
        }
        let Some(work) = work else {
            *slot = Self::Terminal(Instant::now());
            return;
        };
        if let Self::Pending {
            custody: previous, ..
        } = slot
            && matches!(custody, Custody::Execution(_))
        {
            *previous = custody;
        }
        if source == Source::Historical
            && matches!(
                slot,
                Self::Pending {
                    source: Source::Live,
                    ..
                }
            )
        {
            return;
        }
        if let Self::Pending {
            source: previous_source,
            work: previous_work,
            ..
        } = slot
        {
            *previous_source = source;
            *previous_work = work;
        }
    }
}

#[derive(Debug)]
struct Execution {
    owner: CodeModeOwner,
    facts: Vec<CodeModeItem>,
}

impl Operations {
    pub(crate) fn new(capacity: usize) -> Self {
        Self {
            stage: Stage::Unbound,
            active_turn: None,
            closed_turns: VecDeque::default(),
            capacity,
            items: BTreeMap::new(),
            executions: BTreeMap::new(),
        }
    }

    pub(crate) fn reconcile_turn(&mut self, turn: Option<String>) {
        if self.active_turn == turn {
            return;
        }
        if let Some(previous) = self.active_turn.take() {
            self.close_turn(&previous);
            self.reap(&Custody::Turn(previous));
        }
        self.active_turn = turn;
    }

    fn close_turn(&mut self, turn: &str) {
        if !self.closed_turns.iter().any(|closed| closed == turn) {
            self.closed_turns.push_back(turn.to_owned());
        }
        while self.closed_turns.len() > self.capacity {
            let _ = self.closed_turns.pop_front();
        }
    }

    fn reap(&mut self, owner: &Custody) {
        for revision in self.items.values_mut() {
            if matches!(revision, Revision::Pending { custody, .. } if custody == owner) {
                *revision = Revision::Terminal(Instant::now());
            }
        }
    }

    fn trim(&mut self) {
        trim_revisions(&mut self.items, self.capacity);
        for execution in self.executions.values_mut() {
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
            .filter(|(_, execution)| execution.finished())
            .map(|(id, execution)| (execution.owner.started_at_ms, id.clone()))
            .collect::<Vec<_>>();
        retired.sort_unstable();
        let excess = retired.len().saturating_sub(self.capacity);
        for (_, id) in retired.into_iter().take(excess) {
            let _ = self.executions.remove(&id);
        }
    }

    pub(crate) fn reap_connection(&mut self) {
        self.items
            .retain(|_, revision| matches!(revision, Revision::Terminal(_)));
        self.executions.retain(|_, execution| execution.finished());
        self.active_turn = None;
    }

    pub(crate) fn refresh(&mut self, thread_id: ThreadId, turns: &[Turn]) {
        if !matches!(self.stage, Stage::Attached(id) | Stage::Closed(id) if id == thread_id) {
            *self = Self::new(self.capacity);
        }
        self.reap_connection();
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
                        && !finished.contains_key(&owned.owner.exec_call_id)
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
                self.reconcile_turn(Some(n.turn.id.clone()));
                for item in &n.turn.items {
                    self.direct_item(&n.turn.id, item, Source::Historical, None);
                }
            }
            ServerNotification::TurnCompleted(n) => {
                self.close_turn(&n.turn.id);
                self.reap(&Custody::Turn(n.turn.id.clone()));
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
                    let _ = self
                        .items
                        .insert(id.clone(), Revision::Terminal(Instant::now()));
                }
            }
            ServerNotification::ThreadClosed(_) => {
                self.stage = Stage::Closed(thread_id);
                self.active_turn = None;
                self.items.clear();
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
        if pending(item, started) == Some(true) && !self.accepts_turn(turn) {
            return;
        }
        self.item(Custody::Turn(turn.to_owned()), item, source, started);
    }

    fn item(&mut self, custody: Custody, item: &ThreadItem, source: Source, started: Option<bool>) {
        if let Some(call) = output_call(item) {
            finish_call(&mut self.items, &call.to_owned());
        } else if let Some(pending) = pending(item, started) {
            admit(
                &mut self.items,
                item.id().to_owned(),
                pending.then(|| Pending::Item(Box::new(item.clone()))),
                source,
                custody,
            );
        }
    }

    fn call(&mut self, custody: Custody, id: String, name: String, source: Source) {
        match self.items.entry(id) {
            Entry::Vacant(slot) => {
                let _ = slot.insert(Revision::Pending {
                    custody,
                    source,
                    work: Pending::Call(name),
                });
            }
            Entry::Occupied(mut slot) => {
                if let Revision::Pending {
                    custody: previous, ..
                } = slot.get_mut()
                    && matches!(custody, Custody::Execution(_))
                {
                    *previous = custody;
                }
            }
        }
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
                self.call(
                    Custody::Turn(turn.to_owned()),
                    call_id.clone(),
                    namespace
                        .as_ref()
                        .map_or_else(|| name.clone(), |namespace| format!("{namespace}.{name}")),
                    Source::Live,
                );
            }
            ResponseItem::FunctionCallOutput {
                call_id: Some(call),
                ..
            }
            | ResponseItem::CustomToolCallOutput { call_id: call, .. } => {
                finish_call(&mut self.items, call);
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
            execution.owner.cell_id.clone_from(&owner.cell_id);
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
            if !matches!(fact.event, CodeModeEvent::ToolCall { .. }) {
                insert_code_mode_fact(&mut execution.facts, fact.clone());
            }
            let finished = execution.finished();
            let custody = Custody::Execution(owner.exec_call_id.clone());
            match &fact.event {
                CodeModeEvent::ToolCall { call_id } => {
                    if finished {
                        let _ = self
                            .items
                            .insert(call_id.clone(), Revision::Terminal(Instant::now()));
                    } else {
                        self.call(custody, call_id.clone(), "Tool call".to_owned(), source);
                    }
                }
                CodeModeEvent::WaitCompleted { call_id, .. } => {
                    finish_call(&mut self.items, call_id);
                }
                _ => {}
            }
            if finished {
                self.reap(&Custody::Execution(owner.exec_call_id.clone()));
            }
            // The typed execution supersedes the provider call, without deleting its capture.
            finish_call(&mut self.items, &owner.exec_call_id);
        } else if execution.finished() && pending(item, started) == Some(true) {
            let _ = self
                .items
                .insert(item.id().to_owned(), Revision::Terminal(Instant::now()));
        } else {
            self.item(
                Custody::Execution(owner.exec_call_id.clone()),
                item,
                source,
                started,
            );
        }
    }

    pub(crate) fn rows(&self, thread_id: ThreadId) -> Vec<Row> {
        if !matches!(self.stage, Stage::Attached(id) if id == thread_id) {
            return Vec::new();
        }
        let mut rows = self
            .items
            .iter()
            .filter_map(|(id, revision)| {
                let Revision::Pending {
                    custody: Custody::Turn(turn),
                    ..
                } = revision
                else {
                    return None;
                };
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
                .flat_map(|execution| execution.rows(&self.items)),
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

    fn rows(&self, items: &BTreeMap<String, Revision>) -> Vec<Row> {
        let owner = &self.owner;
        let finished = self.finished();
        let mut children = items
            .iter()
            .filter(|(_, revision)| matches!(revision, Revision::Pending { custody: Custody::Execution(id), .. } if id == &owner.exec_call_id))
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
                line(format!("Waiting for ({})", code_mode_owner_label(target))),
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
            crate::render::header::leading(
                "Running",
                line(format!(" ({})", code_mode_owner_label(owner))),
            ),
            vec![line(format!("Call {}", owner.exec_call_id))],
        );
        row.children = children;
        vec![row]
    }
}

fn admit(
    items: &mut BTreeMap<String, Revision>,
    key: String,
    work: Option<Pending>,
    source: Source,
    custody: Custody,
) {
    match items.entry(key) {
        Entry::Occupied(mut slot) => Revision::admit(slot.get_mut(), work, source, custody),
        Entry::Vacant(slot) => {
            let _ = slot.insert(work.map_or_else(
                || Revision::Terminal(Instant::now()),
                |work| Revision::Pending {
                    custody,
                    source,
                    work,
                },
            ));
        }
    }
}

fn trim_revisions<K: Ord + Clone>(items: &mut BTreeMap<K, Revision>, capacity: usize) {
    let mut retired = items
        .iter()
        .filter_map(|(key, revision)| match revision {
            Revision::Terminal(at) => Some((*at, key.clone())),
            Revision::Pending { .. } => None,
        })
        .collect::<Vec<_>>();
    retired.sort_unstable();
    let excess = retired.len().saturating_sub(capacity);
    for (_, key) in retired.into_iter().take(excess) {
        let _ = items.remove(&key);
    }
}

fn finish_call<K: Ord + Clone>(items: &mut BTreeMap<K, Revision>, key: &K) {
    match items.entry(key.clone()) {
        Entry::Vacant(slot) => {
            let _ = slot.insert(Revision::Terminal(Instant::now()));
        }
        Entry::Occupied(mut slot) => {
            if matches!(
                slot.get(),
                Revision::Pending {
                    work: Pending::Call(_),
                    ..
                }
            ) {
                let _ = slot.insert(Revision::Terminal(Instant::now()));
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
                let command = strip_bash_lc_and_escape(&split_command_string(command));
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
    Some(row(thread, key, id, line(summary), details))
}

fn row(
    thread_id: ThreadId,
    key: Key,
    item_id: &str,
    summary: Line<'static>,
    details: Vec<Line<'static>>,
) -> Row {
    Row {
        key,
        summary,
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
