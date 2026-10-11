mod delivery;
mod tools;
mod wire;

use std::collections::BTreeMap;
use std::collections::HashMap;
use std::sync::Arc;
use std::sync::Mutex;
use std::time::SystemTime;

use codex_app_server_protocol::AlarmSummary;
use codex_app_server_protocol::ServerNotification;
use codex_app_server_protocol::ThreadAlarmsUpdatedNotification;
use codex_app_server_protocol::alarm_sounding::AlarmCause;
use codex_app_server_protocol::alarm_sounding::AlarmSounding;
use codex_core::CodexThread;
use codex_core::TerminalWatch;
use codex_protocol::ThreadId;
use codex_rollout::state_db::StateDbHandle;
use codex_state::AlarmTrigger;
use codex_state::ThreadAlarm;
use codex_state::ThreadAlarmState;
use tokio::sync::Notify;
use tokio::sync::OnceCell;
use tokio::sync::mpsc;
use tokio::task::JoinHandle;

use crate::outgoing_message::ConnectionId;
use crate::outgoing_message::OutgoingMessageSender;
use crate::thread_state::ThreadRuntimeLease;
use crate::thread_tools::CoordinationSlot;

pub(crate) use tools::install;

#[derive(Clone)]
pub(crate) struct Alarms(Arc<Shared>);

enum Arm {
    Clock {
        at: i64,
    },
    Watch {
        session_id: i32,
        watch: TerminalWatch,
        lease: ThreadRuntimeLease,
        deadline: Option<i64>,
    },
}

pub(crate) struct AlarmWorker {
    alarms: Alarms,
    tasks: Vec<JoinHandle<()>>,
    start: tokio::sync::oneshot::Sender<()>,
}

impl AlarmWorker {
    pub(crate) fn start(self) -> RunningAlarmWorker {
        let _ = self.start.send(());
        RunningAlarmWorker {
            alarms: self.alarms,
            tasks: self.tasks,
        }
    }
}

pub(crate) struct RunningAlarmWorker {
    alarms: Alarms,
    tasks: Vec<JoinHandle<()>>,
}

impl Drop for RunningAlarmWorker {
    fn drop(&mut self) {
        for task in &self.tasks {
            task.abort();
        }
        self.alarms.0.registry().watches.clear();
    }
}

#[derive(Default)]
struct Registry {
    alarms: BTreeMap<i64, ThreadAlarm>,
    watches: HashMap<i64, WatchTask>,
}

struct WatchTask {
    task: JoinHandle<()>,
    _lease: ThreadRuntimeLease,
}

impl Drop for WatchTask {
    fn drop(&mut self) {
        self.task.abort();
    }
}

struct Store {
    db: StateDbHandle,
    owner: codex_install_context::Channel,
}

struct Shared {
    store: Option<Store>,
    coordination: CoordinationSlot,
    registry: Mutex<Registry>,
    mutation: tokio::sync::Semaphore,
    initialized: OnceCell<()>,
    changed: Notify,
    sounded: mpsc::UnboundedSender<(i64, AlarmCause)>,
    publications: mpsc::UnboundedSender<Publication>,
}

impl Shared {
    #[expect(
        clippy::expect_used,
        reason = "Poisoning violates the alarm registry invariant; recovery is impossible."
    )]
    fn registry(&self) -> std::sync::MutexGuard<'_, Registry> {
        self.registry.lock().expect("alarm registry poisoned")
    }

    #[expect(
        clippy::expect_used,
        reason = "The private mutation semaphore is never closed."
    )]
    async fn mutate(&self) -> tokio::sync::SemaphorePermit<'_> {
        self.mutation
            .acquire()
            .await
            .expect("alarm mutation semaphore closed")
    }
}

enum Publication {
    Broadcast(ServerNotification),
    To(ConnectionId, ServerNotification),
}

impl Alarms {
    pub(crate) fn spawn(
        owner: Option<codex_install_context::Channel>,
        db: Option<StateDbHandle>,
        coordination: CoordinationSlot,
        outgoing: Arc<OutgoingMessageSender>,
    ) -> (Self, AlarmWorker) {
        let (publications, mut pending_publications) = mpsc::unbounded_channel();
        let (sounded, sounds) = mpsc::unbounded_channel();
        let service = Self(Arc::new(Shared {
            store: owner.zip(db).map(|(owner, db)| Store { db, owner }),
            coordination,
            registry: Mutex::default(),
            mutation: tokio::sync::Semaphore::new(1),
            initialized: OnceCell::new(),
            changed: Notify::new(),
            sounded,
            publications,
        }));
        let (start, ready) = tokio::sync::oneshot::channel();
        let tasks = if service.available() {
            let publisher = tokio::spawn(async move {
                while let Some(publication) = pending_publications.recv().await {
                    match publication {
                        Publication::Broadcast(notification) => {
                            outgoing.send_server_notification(notification).await
                        }
                        Publication::To(connection, notification) => {
                            outgoing
                                .send_server_notification_to_connections(
                                    &[connection],
                                    notification,
                                )
                                .await
                        }
                    }
                }
            });
            let alarms = service.clone();
            let scheduler = tokio::spawn(async move {
                if ready.await.is_ok() {
                    delivery::run(alarms, sounds).await;
                }
            });
            vec![scheduler, publisher]
        } else {
            Vec::new()
        };
        let worker = AlarmWorker {
            alarms: service.clone(),
            tasks,
            start,
        };
        (service, worker)
    }

    async fn initialize(&self) -> Result<(), String> {
        self.0
            .initialized
            .get_or_try_init(|| async {
                let Some(store) = &self.0.store else {
                    return Ok(());
                };
                let _mutation = self.0.mutate().await;
                let saved = store
                    .db
                    .all_thread_alarms(store.owner)
                    .await
                    .map_err(|err| err.to_string())?;
                self.0.registry().alarms =
                    saved.into_iter().map(|alarm| (alarm.id, alarm)).collect();
                let recovered_at = now();
                let ready = self
                    .0
                    .registry()
                    .alarms
                    .values()
                    .filter_map(|alarm| match &alarm.state {
                        ThreadAlarmState::Armed(AlarmTrigger::Watch {
                            session_id,
                            command,
                            ..
                        }) => Some((
                            alarm.id,
                            AlarmCause::Lost {
                                session_id: *session_id,
                                command: command.clone(),
                            },
                        )),
                        ThreadAlarmState::Armed(AlarmTrigger::Clock { at })
                            if *at <= recovered_at =>
                        {
                            Some((
                                alarm.id,
                                AlarmCause::Clock {
                                    late_by_seconds: (recovered_at.saturating_sub(*at) > 60_000)
                                        .then(|| {
                                            recovered_at.saturating_sub(*at).unsigned_abs() / 1000
                                        }),
                                },
                            ))
                        }
                        ThreadAlarmState::Armed(AlarmTrigger::Clock { .. })
                        | ThreadAlarmState::Sounded { .. } => None,
                    })
                    .collect::<Vec<_>>();
                for (id, cause) in ready {
                    self.sound_locked(id, cause).await?;
                }
                Ok(())
            })
            .await
            .copied()
    }

    fn store(&self) -> Result<&Store, String> {
        self.0.store.as_ref().ok_or_else(|| {
            "Alarms are available only in the managed daemon with durable storage.".into()
        })
    }

    pub(crate) fn available(&self) -> bool {
        self.0.store.is_some()
    }

    fn held(&self, thread: ThreadId) -> bool {
        self.0.registry().alarms.values().any(|alarm| {
            alarm.thread_id == thread && matches!(alarm.state, ThreadAlarmState::Armed(_))
        })
    }

    fn attach_hold(&self, thread: ThreadId, data: &codex_extension_api::ExtensionData) {
        let weak = Arc::downgrade(&self.0);
        data.insert(codex_core::idle_hold::IdleHold::new(move || {
            weak.upgrade()
                .is_some_and(|shared| Alarms(shared).held(thread))
        }));
        data.insert(self.clone());
    }

    fn summaries(&self, thread: ThreadId) -> Vec<AlarmSummary> {
        let mut alarms = self
            .0
            .registry()
            .alarms
            .values()
            .filter(|alarm| alarm.thread_id == thread)
            .filter_map(wire::summary)
            .collect::<Vec<_>>();
        alarms.sort_by_key(|alarm| {
            (
                match &alarm.condition {
                    codex_app_server_protocol::AlarmCondition::Clock { at } => *at,
                    codex_app_server_protocol::AlarmCondition::Watch { deadline, .. } => {
                        deadline.unwrap_or(i64::MAX)
                    }
                },
                alarm.armed_at,
            )
        });
        alarms
    }

    fn publish(&self, thread: ThreadId, connection: Option<ConnectionId>) {
        let notification =
            ServerNotification::ThreadAlarmsUpdated(ThreadAlarmsUpdatedNotification {
                thread_id: thread.to_string(),
                alarms: self.summaries(thread),
            });
        let publication = match connection {
            Some(connection) => Publication::To(connection, notification),
            None => Publication::Broadcast(notification),
        };
        let _ = self.0.publications.send(publication);
        self.0.changed.notify_one();
    }

    async fn arm(&self, thread: ThreadId, note: &str, arm: Arm) -> Result<ThreadAlarm, String> {
        self.initialize().await?;
        let _mutation = self.0.mutate().await;
        let trigger = match &arm {
            Arm::Clock { at } => AlarmTrigger::Clock { at: *at },
            Arm::Watch {
                session_id,
                watch,
                deadline,
                ..
            } => AlarmTrigger::Watch {
                session_id: *session_id,
                command: watch.command().to_string(),
                pattern: watch.pattern().map(str::to_string),
                deadline: *deadline,
            },
        };
        let store = self.store()?;
        let alarm = store
            .db
            .arm_thread_alarm(store.owner, thread, note, now(), trigger)
            .await
            .map_err(|err| err.to_string())?;
        let id = alarm.id;
        self.0.registry().alarms.insert(id, alarm.clone());
        if let Arm::Watch {
            session_id,
            watch,
            lease,
            deadline,
        } = arm
        {
            let sounded = self.0.sounded.clone();
            let task = tokio::spawn(async move {
                let cause = delivery::watch(watch, session_id, deadline).await;
                let _ = sounded.send((id, cause));
            });
            self.0.registry().watches.insert(
                id,
                WatchTask {
                    task,
                    _lease: lease,
                },
            );
        }
        self.publish(thread, None);
        Ok(alarm)
    }

    pub(crate) async fn disarm(
        &self,
        thread: ThreadId,
        id: &str,
    ) -> Result<Vec<AlarmSummary>, String> {
        self.initialize().await?;
        let _mutation = self.0.mutate().await;
        let parsed = id
            .strip_prefix('a')
            .filter(|digits| {
                !digits.is_empty()
                    && digits.len() <= 19
                    && digits.bytes().all(|byte| byte.is_ascii_digit())
            })
            .and_then(|id| id.parse().ok());
        let label = id.chars().take(40).collect::<String>();
        let store = self.store()?;
        let found = match parsed {
            Some(id) => store
                .db
                .disarm_thread_alarm(store.owner, thread, id)
                .await
                .map_err(|err| err.to_string())?,
            None => false,
        };
        if !found {
            return Err(format!(
                "Unknown armed alarm {label}.\n{}",
                wire::armed_list(&self.summaries(thread))
            ));
        }
        if let Some(id) = parsed {
            let mut registry = self.0.registry();
            registry.alarms.remove(&id);
            registry.watches.remove(&id);
        }
        self.publish(thread, None);
        Ok(self.summaries(thread))
    }

    pub(crate) async fn archive_guard(&self) -> Result<tokio::sync::SemaphorePermit<'_>, String> {
        self.initialize().await?;
        Ok(self.0.mutate().await)
    }

    pub(crate) async fn discard_locked(&self, thread: ThreadId) -> Result<(), String> {
        let Some(store) = &self.0.store else {
            return Ok(());
        };
        store
            .db
            .delete_thread_alarms(store.owner, thread)
            .await
            .map_err(|err| err.to_string())?;
        let mut registry = self.0.registry();
        let ids = registry
            .alarms
            .values()
            .filter(|alarm| alarm.thread_id == thread)
            .map(|alarm| alarm.id)
            .collect::<Vec<_>>();
        for id in ids {
            registry.alarms.remove(&id);
            registry.watches.remove(&id);
        }
        drop(registry);
        self.publish(thread, None);
        Ok(())
    }

    async fn sound_locked(&self, id: i64, cause: AlarmCause) -> Result<(), String> {
        let alarm = self.0.registry().alarms.get(&id).cloned();
        let Some(alarm) = alarm else {
            return Ok(());
        };
        if matches!(alarm.state, ThreadAlarmState::Sounded { .. }) {
            return Ok(());
        }
        let sounding = AlarmSounding {
            id: format!("a{id}"),
            armed_at: alarm.armed_at,
            sounded_at: now(),
            note: alarm.note,
            cause,
            still_armed: self
                .summaries(alarm.thread_id)
                .into_iter()
                .filter(|alarm| alarm.id != format!("a{id}"))
                .map(|alarm| alarm.id)
                .collect(),
        };
        let body = sounding.render();
        let store = self.store()?;
        let sounded = store
            .db
            .sound_thread_alarm(store.owner, alarm.thread_id, id, &body)
            .await
            .map_err(|err| err.to_string())?;
        let mut registry = self.0.registry();
        registry.watches.remove(&id);
        if sounded {
            registry.alarms.insert(
                id,
                ThreadAlarm {
                    note: sounding.note,
                    state: ThreadAlarmState::Sounded { body },
                    ..alarm
                },
            );
        } else {
            registry.alarms.remove(&id);
        }
        drop(registry);
        self.publish(alarm.thread_id, None);
        Ok(())
    }
}

pub(crate) async fn publish_to(
    connection: ConnectionId,
    thread_id: ThreadId,
    thread: &CodexThread,
) {
    if let Some(alarms) = thread.thread_extension_data().get::<Alarms>() {
        if let Err(error) = alarms.initialize().await {
            tracing::warn!(%error, "cannot publish alarms");
            return;
        }
        let _mutation = alarms.0.mutate().await;
        alarms.publish(thread_id, Some(connection));
    }
}

fn now() -> i64 {
    SystemTime::now()
        .duration_since(SystemTime::UNIX_EPOCH)
        .map_or(0, |elapsed| {
            i64::try_from(elapsed.as_millis()).unwrap_or(i64::MAX)
        })
}

pub(crate) async fn disarm_request(
    alarms: &Alarms,
    params: codex_app_server_protocol::ThreadAlarmDisarmParams,
) -> Result<
    Option<codex_app_server_protocol::ClientResponsePayload>,
    codex_app_server_protocol::JSONRPCErrorError,
> {
    let id = ThreadId::from_string(&params.thread_id)
        .map_err(|err| crate::error_code::invalid_request(format!("invalid thread id: {err}")))?;
    let alarms = alarms
        .disarm(id, &params.id)
        .await
        .map_err(crate::error_code::invalid_request)?;
    Ok(Some(
        codex_app_server_protocol::ThreadAlarmDisarmResponse { alarms }.into(),
    ))
}
