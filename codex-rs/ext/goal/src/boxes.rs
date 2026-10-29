//! Goal boxes (codex-1337): the timebox and quotabox a user sets on a root agent's goal.
//!
//! Reaching a box halts the goal: it pauses, and the agent is asked to stop work gracefully, on
//! its honor. A timebox also reminds the agent each time its time left halves.

use std::fmt;
use std::future::Future;
use std::pin::Pin;
use std::sync::Arc;
use std::sync::LazyLock;
use std::sync::OnceLock;

use chrono::DateTime;
use chrono::Local;
use chrono::Utc;
use codex_protocol::ThreadId;
use codex_protocol::goal_boxes::GoalBoxes;
use codex_protocol::goal_boxes::GoalTimebox;
use codex_protocol::goal_boxes::QuotaGauge;
use codex_protocol::goal_boxes::QuotaReading;
use codex_protocol::goal_boxes::Quotabox;
use codex_protocol::models::ResponseItem;
use codex_protocol::protocol::ThreadGoal;
use codex_utils_template::Template;

use crate::steering::escape_xml_text;
use crate::steering::goal_context_input_item;
use crate::steering::parse_embedded_template;

/// A future a [`GoalBoxHost`] returns.
pub type BoxFuture<'a, T> = Pin<Box<dyn Future<Output = T> + Send + 'a>>;

/// Why the quota a quotabox watches cannot be read.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum QuotaError {
    /// The thread's provider reports no quota a box can watch.
    Unsupported(String),
    /// The quota exists but cannot be read now.
    Unreadable(String),
}

impl fmt::Display for QuotaError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Unsupported(message) | Self::Unreadable(message) => f.write_str(message),
        }
    }
}

/// What the server lends goal boxes.
pub trait GoalBoxHost: Send + Sync {
    /// The quota of the provider account a root thread's requests draw on.
    fn quota(&self, thread_id: ThreadId) -> BoxFuture<'_, Result<QuotaReading, QuotaError>>;
    /// Tells a thread's clients something about its goal boxes, such as a halt.
    fn announce(&self, thread_id: ThreadId, message: String) -> BoxFuture<'_, ()>;
}

/// The host, installed once the server's request processors exist.
#[derive(Clone, Default)]
pub(crate) struct GoalBoxHostSlot(Arc<OnceLock<Arc<dyn GoalBoxHost>>>);

impl GoalBoxHostSlot {
    pub(crate) fn install(&self, host: Arc<dyn GoalBoxHost>) {
        if self.0.set(host).is_err() {
            tracing::warn!("goal box host installed twice; keeping the first");
        }
    }

    pub(crate) fn host(&self) -> Option<Arc<dyn GoalBoxHost>> {
        self.0.get().cloned()
    }
}

impl fmt::Debug for GoalBoxHostSlot {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_tuple("GoalBoxHostSlot")
            .field(&self.0.get().is_some())
            .finish()
    }
}

/// A goal's boxes with the quota its quotabox would watch.
#[derive(Debug, Clone, PartialEq)]
pub struct GoalBoxesReport {
    pub boxes: GoalBoxes,
    /// The quota as just read; `None` when nothing read it.
    pub gauge: Option<QuotaGauge>,
}

pub(crate) fn gauge(reading: Result<QuotaReading, QuotaError>) -> QuotaGauge {
    match reading {
        Ok(reading) => QuotaGauge::Read { reading },
        Err(error) => QuotaGauge::Unavailable {
            message: error.to_string(),
        },
    }
}

/// Shortest gap between a timebox's reminders, counting its halt as the last of them.
const REMINDER_GAP_SECONDS: i64 = 10 * 60;

/// A timebox's reminder instants in order, one each time its time left halves. Each gap equals
/// the time left at the later reminder, so keeping only reminders with at least
/// [`REMINDER_GAP_SECONDS`] left keeps every gap, the last before the halt included, that long.
pub(crate) fn reminder_instants(timebox: GoalTimebox) -> impl Iterator<Item = i64> {
    std::iter::successors(Some((timebox.deadline - timebox.set_at) / 2), |left| {
        Some(left / 2)
    })
    .take_while(|left| *left >= REMINDER_GAP_SECONDS)
    .map(move |left| timebox.deadline - left)
}

/// A box reached: why a goal halts.
#[derive(Debug, Clone, Copy, PartialEq)]
pub(crate) enum Halt {
    TimeboxEnded {
        deadline: i64,
    },
    QuotaboxReached {
        quotabox: Quotabox,
        reading: QuotaReading,
    },
}

impl Halt {
    /// The halt `boxes` call for at `now`, judged by the latest quota `reading`.
    pub(crate) fn due(boxes: GoalBoxes, now: i64, reading: Option<QuotaReading>) -> Option<Self> {
        if let Some(timebox) = boxes.timebox
            && now >= timebox.deadline
        {
            return Some(Self::TimeboxEnded {
                deadline: timebox.deadline,
            });
        }
        let (quotabox, reading) = boxes.quotabox.zip(reading)?;
        quotabox
            .reached_by(reading)?
            .then_some(Self::QuotaboxReached { quotabox, reading })
    }

    /// The halt as its thread's clients hear it.
    pub(crate) fn announcement(self) -> String {
        match self {
            Self::TimeboxEnded { deadline } => {
                format!(
                    "Goal paused: its timebox ended at {}.",
                    local_time(deadline)
                )
            }
            Self::QuotaboxReached { quotabox, reading } => format!(
                "Goal paused: quota fell to {reading}, reaching its quotabox of {quotabox}."
            ),
        }
    }

    /// Why a goal with this halt due cannot become active.
    pub(crate) fn refusal(self) -> String {
        match self {
            Self::TimeboxEnded { deadline } => format!(
                "The goal's timebox ended at {}; extend or clear it first.",
                local_time(deadline)
            ),
            Self::QuotaboxReached { quotabox, reading } => format!(
                "Quota is at {reading}, within the goal's quotabox of {quotabox}; lower or clear it first."
            ),
        }
    }

    fn cause(self) -> String {
        match self {
            Self::TimeboxEnded { deadline } => {
                format!("the timebox the user set ended at {}", local_time(deadline))
            }
            Self::QuotaboxReached { quotabox, reading } => {
                format!("quota fell to {reading}, reaching the quotabox the user set at {quotabox}")
            }
        }
    }
}

static HALT_TEMPLATE: LazyLock<Template> = LazyLock::new(|| {
    parse_embedded_template(include_str!("../templates/goals/halt.md"), "goals/halt.md")
});

static TIMEBOX_REMINDER_TEMPLATE: LazyLock<Template> = LazyLock::new(|| {
    parse_embedded_template(
        include_str!("../templates/goals/timebox_reminder.md"),
        "goals/timebox_reminder.md",
    )
});

/// Asks the agent to stop work because `halt` paused its goal.
pub(crate) fn halt_item(goal: &ThreadGoal, halt: Halt) -> Option<ResponseItem> {
    let objective = escape_xml_text(&goal.objective);
    let cause = halt.cause();
    HALT_TEMPLATE
        .render([("cause", cause.as_str()), ("objective", objective.as_str())])
        .inspect_err(|err| tracing::error!("embedded goals/halt.md failed to render: {err}"))
        .ok()
        .map(goal_context_input_item)
}

/// Tells the agent how much of its timebox is left at `now`.
pub(crate) fn timebox_reminder_item(timebox: GoalTimebox, now: i64) -> Option<ResponseItem> {
    let left = span_text(timebox.deadline - now);
    let span = span_text(timebox.deadline - timebox.set_at);
    let deadline = local_time(timebox.deadline);
    TIMEBOX_REMINDER_TEMPLATE
        .render([
            ("left", left.as_str()),
            ("span", span.as_str()),
            ("deadline", deadline.as_str()),
        ])
        .inspect_err(|err| {
            tracing::error!("embedded goals/timebox_reminder.md failed to render: {err}");
        })
        .ok()
        .map(goal_context_input_item)
}

pub(crate) fn unix_now() -> i64 {
    Utc::now().timestamp()
}

/// `unix` seconds as local wall-clock time.
fn local_time(unix: i64) -> String {
    DateTime::from_timestamp(unix, 0).map_or_else(
        || unix.to_string(),
        |time| {
            time.with_timezone(&Local)
                .format("%Y-%m-%d %H:%M %:z")
                .to_string()
        },
    )
}

/// `seconds` in hours and minutes, or in minutes and seconds under an hour.
fn span_text(seconds: i64) -> String {
    let seconds = seconds.max(0);
    match (seconds / 3600, seconds % 3600 / 60, seconds % 60) {
        (0, 0, seconds) => format!("{seconds}s"),
        (0, minutes, 0) => format!("{minutes}m"),
        (0, minutes, seconds) => format!("{minutes}m {seconds}s"),
        (hours, 0, _) => format!("{hours}h"),
        (hours, minutes, _) => format!("{hours}h {minutes}m"),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn timebox_reminders_halve_the_time_left_until_ten_minutes_remain() {
        let left = |timebox: GoalTimebox| {
            reminder_instants(timebox)
                .map(|instant| timebox.deadline - instant)
                .collect::<Vec<_>>()
        };
        let twelve_hours = GoalTimebox {
            set_at: 1_000,
            deadline: 1_000 + 12 * 3600,
        };
        assert_eq!(
            left(twelve_hours),
            [21_600, 10_800, 5_400, 2_700, 1_350, 675]
        );
        // The halt counts as the last reminder: none comes within ten minutes of it.
        assert_eq!(
            left(GoalTimebox {
                set_at: 0,
                deadline: 1_200
            }),
            [600]
        );
        assert_eq!(
            left(GoalTimebox {
                set_at: 0,
                deadline: 1_199
            }),
            [0_i64; 0]
        );
    }
}
