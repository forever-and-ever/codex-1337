//! Goal boxes (codex-1337): limits a user sets on a root agent's goal. Reaching one halts the
//! goal: it pauses, and its agent is asked to stop work gracefully, on its honor.

use std::fmt;

use schemars::JsonSchema;
use serde::Deserialize;
use serde::Serialize;
use ts_rs::TS;

use crate::Usd;

/// A goal's boxes; an absent box is not set.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Serialize, Deserialize, JsonSchema, TS)]
#[serde(rename_all = "camelCase")]
pub struct GoalBoxes {
    pub timebox: Option<GoalTimebox>,
    pub quotabox: Option<Quotabox>,
}

impl GoalBoxes {
    pub fn is_empty(self) -> bool {
        self.timebox.is_none() && self.quotabox.is_none()
    }
}

/// The wall-clock span a goal may run, in Unix seconds.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, JsonSchema, TS)]
#[serde(rename_all = "camelCase")]
pub struct GoalTimebox {
    /// When the timebox was set; its reminders halve the time left from here.
    pub set_at: i64,
    /// When the goal halts.
    pub deadline: i64,
}

/// The level of the root agent's provider quota at which a goal halts.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, JsonSchema, TS)]
#[serde(tag = "type", rename_all = "camelCase")]
pub enum Quotabox {
    /// Percent of a subscription's weekly window left, 1 through 99.
    WeeklyPercentLeft { percent: u8 },
    /// Dollars an `OpenRouter` key can still spend.
    UsdAvailable { usd: Usd },
}

impl Quotabox {
    /// Whether `reading` is at or below this box, or `None` when it measures another quota.
    pub fn reached_by(self, reading: QuotaReading) -> Option<bool> {
        match (self, reading) {
            (
                Self::WeeklyPercentLeft { percent: level },
                QuotaReading::WeeklyPercentLeft { percent },
            ) => Some(percent <= f64::from(level)),
            (Self::UsdAvailable { usd: level }, QuotaReading::UsdAvailable { usd }) => {
                Some(usd <= level)
            }
            (Self::WeeklyPercentLeft { .. }, QuotaReading::UsdAvailable { .. })
            | (Self::UsdAvailable { .. }, QuotaReading::WeeklyPercentLeft { .. }) => None,
        }
    }
}

/// A reading of the quota a quotabox watches.
#[derive(Debug, Clone, Copy, PartialEq, Serialize, Deserialize, JsonSchema, TS)]
#[serde(tag = "type", rename_all = "camelCase")]
pub enum QuotaReading {
    WeeklyPercentLeft { percent: f64 },
    UsdAvailable { usd: Usd },
}

/// The quota a goal's quotabox watches, as just read.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, JsonSchema, TS)]
#[serde(tag = "type", rename_all = "camelCase")]
pub enum QuotaGauge {
    Read { reading: QuotaReading },
    Unavailable { message: String },
}

impl fmt::Display for Quotabox {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::WeeklyPercentLeft { percent } => write!(f, "{percent}% of the weekly quota left"),
            Self::UsdAvailable { usd } => write!(f, "{usd} available"),
        }
    }
}

impl fmt::Display for QuotaReading {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::WeeklyPercentLeft { percent } => {
                write!(f, "{percent:.1}% of the weekly quota left")
            }
            Self::UsdAvailable { usd } => write!(f, "{usd} available"),
        }
    }
}
